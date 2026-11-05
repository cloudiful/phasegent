//! Deterministic full-CLI Forgejo tests (issue 1201 P3).
//!
//! These drive the real `issue create` / `issue close` executors against a
//! local mock HTTP server and an isolated SQLite lease store, so the
//! scoped-acquisition and exact-scope-release wiring is proven end to end
//! rather than only through the manual provider/CLI seams.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::thread::{self, JoinHandle};

use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::worktree::leases::{
    NewLease, ensure_schema, insert_lease, insert_lease_scoped, list_all_for_repo,
};
use crate::worktree::{LeaseScope, ProcessWorktreeRunner, repo_identity};

const REPO_JSON: &str = r#"{"id":17,"name":"repo","full_name":"owner/repo"}"#;
const LABELS_JSON: &str = r#"[{"id":1,"name":"status/New"},{"id":2,"name":"status/In Progress"},{"id":3,"name":"status/In Review"},{"id":4,"name":"status/Resolved"},{"id":5,"name":"status/Blocked"},{"id":6,"name":"status/Changes Requested"},{"id":7,"name":"status/Cancelled"}]"#;

fn issue_json(number: u64, state: &str, labels: &str) -> String {
    format!(
        r#"{{"id":{},"number":{number},"title":"fixture","body":"fixture","state":"{state}","html_url":"https://example.test/owner/repo/issues/{number}","labels":{labels}}}"#,
        500 + number
    )
}

struct Mock {
    base: String,
    task: JoinHandle<Vec<String>>,
}

impl Mock {
    fn start(responses: Vec<String>) -> Self {
        Self::start_status(responses.into_iter().map(|body| (200, body)).collect())
    }

    fn start_status(responses: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let _expected = responses.len();
        let task = thread::spawn(move || {
            let mut requests = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            for (status, body) in responses {
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(pair) => break pair,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if std::time::Instant::now() > deadline {
                                return requests;
                            }
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => return requests,
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let size = stream.read(&mut buffer).unwrap();
                    if size == 0 {
                        break;
                    }
                    data.extend_from_slice(&buffer[..size]);
                    if let Some(end) = data.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8_lossy(&data).into_owned());
                let response = format!(
                    "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    if status == 200 { "OK" } else { "Error" },
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        Self { base, task }
    }

    fn finish(self) -> Vec<String> {
        self.task.join().unwrap()
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = crate::test_scratch::root().join(format!(
            "phasegent-forgejo-cli-{label}-{}-{}",
            std::process::id(),
            crate::worktree::now_unix_secs()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `action` with the process working directory set to `dir`, restoring it
/// on completion. Every caller holds the workflow test lock.
fn in_dir<T>(dir: &Path, action: impl FnOnce() -> T) -> T {
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let result = action();
    std::env::set_current_dir(previous).unwrap();
    result
}

/// A minimal git checkout with one commit on `main`.
fn init_repo(dir: &Path) {
    use crate::git_runner::GitRunner;
    let runner = crate::git_runner::ProcessGitRunner::in_directory(dir);
    runner.run(&["init", "-q", "-b", "main"]).expect("git init");
    runner
        .run(&[
            "-c",
            "user.name=phasegent-test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ])
        .expect("git commit");
}

struct Fixture {
    repo: TempDir,
    db: TempDir,
    _index: TempDir,
    _db_env: EnvGuard,
    _index_env: EnvGuard,
    _local_env: EnvGuard,
    _lock: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let lock = lock_workflow_tests();
        let repo = TempDir::new(&format!("{label}-repo"));
        let db = TempDir::new(&format!("{label}-db"));
        let index = TempDir::new(&format!("{label}-index"));
        let db_env = EnvGuard::set(
            "PHASEGENT_DB_PATH",
            db.path().join("leases.sqlite3").to_string_lossy().as_ref(),
        );
        let index_env = EnvGuard::set(
            "PHASEGENT_INDEX_DB_PATH",
            index
                .path()
                .join("index.sqlite3")
                .to_string_lossy()
                .as_ref(),
        );
        let local_env = EnvGuard::set(
            "PHASEGENT_LOCAL_DB_PATH",
            db.path().join("local.sqlite3").to_string_lossy().as_ref(),
        );
        init_repo(repo.path());
        Self {
            repo,
            db,
            _index: index,
            _db_env: db_env,
            _index_env: index_env,
            _local_env: local_env,
            _lock: lock,
        }
    }

    fn storage(&self) -> Storage {
        let path = self.db.path().join("leases.sqlite3");
        let storage = Storage::open_at(&path).unwrap();
        ensure_schema(&storage).unwrap();
        storage
            .save_credential(Role::Orchestrator, "forgejo", "fixture-token")
            .unwrap();
        storage
    }

    fn identity(&self) -> String {
        repo_identity(&ProcessWorktreeRunner::new(), self.repo.path()).unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn lease(
        &self,
        storage: &Storage,
        lease_id: &str,
        issue: u64,
        worktree: &Path,
        branch: &str,
        status: &str,
        scope: Option<&LeaseScope>,
    ) {
        let lease = NewLease {
            lease_id,
            identity: &self.identity(),
            issue,
            session: "session-fixture",
            checkout_path: self.repo.path().to_str().unwrap(),
            worktree_path: worktree.to_str().unwrap(),
            branch,
            status,
            created_at: 1,
            heartbeat_at: crate::worktree::now_unix_secs(),
        };
        match scope {
            Some(scope) => insert_lease_scoped(storage, lease, scope).unwrap(),
            None => insert_lease(storage, lease).unwrap(),
        }
    }

    fn worktree(&self, branch: &str) -> PathBuf {
        let target = self.repo.path().join(branch.replace('/', "-"));
        crate::worktree::worktree_add(
            &ProcessWorktreeRunner::new(),
            self.repo.path(),
            &target,
            branch,
        )
        .unwrap();
        target
    }

    /// Link the fixture checkout's current branch to one Forgejo issue, as
    /// the durable branch-binding fixture the scope guard reads.
    fn link(&self, number: u64, project: &str) {
        use crate::branch_links::{IssueKey, LinkParams, link};
        use crate::git_runner::ProcessGitRunner;
        let storage = Storage::open().unwrap();
        crate::branch_links::ensure_schema(&storage.connection).unwrap();
        let runner = ProcessGitRunner::in_directory(self.repo.path());
        let branch = crate::git_runner::current_branch(&runner).unwrap();
        let repo = crate::branch_links::resolve_repo_key(
            None,
            &crate::branch_links::checkout_root(&runner),
        )
        .unwrap();
        link(
            &storage.connection,
            &LinkParams {
                repo_key: &repo.key,
                branch: &branch,
                issue: &IssueKey::new("forgejo", project, number.to_string()).unwrap(),
                issue_number: number,
                source: "fixture",
                now: 1,
            },
        )
        .unwrap();
    }
}

/// Temporarily remove process-wide environment variables this test must not
/// inherit. Callers hold the workflow lock through [`Fixture`].
struct UnsetEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl UnsetEnv {
    fn apply(names: &[&'static str]) -> Self {
        let mut saved = Vec::with_capacity(names.len());
        for &name in names {
            saved.push((name, std::env::var_os(name)));
            unsafe { std::env::remove_var(name) };
        }
        Self(saved)
    }
}

impl Drop for UnsetEnv {
    fn drop(&mut self) {
        for (name, previous) in self.0.drain(..).rev() {
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

#[test]
fn acquire_rejects_incompatible_forgejo_project_before_lease_write() {
    let fixture = Fixture::new("acquire-incompatible-review");
    let storage = fixture.storage();
    let mock = ZeroMock::start();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "--project-id",
        "23",
        "worktree",
        "acquire",
        "--issue",
        "7",
        "--session",
        "fixture-session",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert!(requests.is_empty());
    assert!(
        rows.is_empty(),
        "incompatible Forgejo flags exited {exit} and wrote a lease: {rows:?}"
    );
    assert_ne!(exit, 0, "incompatible Forgejo flags must fail");
}

#[test]
fn acquire_rejects_implicit_forgejo_default_before_lease_write() {
    let fixture = Fixture::new("acquire-implicit-review");
    let storage = fixture.storage();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let args: Vec<String> = [
        "worktree",
        "acquire",
        "--issue",
        "7",
        "--session",
        "fixture-session",
        "--no-sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert!(requests.is_empty());
    assert!(
        rows.is_empty(),
        "implicit Forgejo default exited {exit} and wrote a lease: {rows:?}"
    );
    assert_ne!(exit, 0, "implicit Forgejo default must fail closed");
}

#[test]
fn stored_forgejo_default_is_rejected_for_acquire_and_recovery() {
    let fixture = Fixture::new("stored-default-boundary-review");
    let storage = fixture.storage();
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
        .unwrap();
    let directory = fixture.worktree("phasegent/7-stored-default");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &directory,
        "phasegent/7-stored-default",
        "active",
        None,
    );
    storage
        .connection
        .execute(
            "UPDATE worktree_leases SET heartbeat_at = 1 WHERE lease_id = 'legacy-seven'",
            [],
        )
        .unwrap();
    let before = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let mock = ZeroMock::start();
    let args: Vec<String> = [
        "worktree",
        "acquire",
        "--issue",
        "8",
        "--session",
        "fixture-session",
        "--no-sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let acquire_exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--release-stale",
        "--stale-days",
        "0",
        "--reason",
        "fixture",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let prune_exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let after = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_ne!(acquire_exit, 0);
    assert_ne!(prune_exit, 0);
    assert!(requests.is_empty());
    assert_eq!(after.len(), before.len());
    assert_eq!(after[0].status, before[0].status);
    assert_eq!(after[0].heartbeat_at, before[0].heartbeat_at);
    assert!(directory.exists());
}

#[test]
fn forgejo_close_override_is_rejected_before_existing_taxi_candidate() {
    let fixture = Fixture::new("close-override-taxi-review");
    let storage = fixture.storage();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    fixture.link(8, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 8, "retained");
    let before = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let mock = ZeroMock::start();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "--close-status-id",
        "7",
        "worktree",
        "acquire",
        "--issue",
        "9",
        "--session",
        "fixture-session",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let after = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_ne!(exit, 0);
    assert!(requests.is_empty());
    assert_eq!(after.len(), before.len());
    assert_eq!(after[0].status, before[0].status);
    assert_eq!(after[0].heartbeat_at, before[0].heartbeat_at);
    assert!(directory.exists());
}

#[cfg(unix)]
#[test]
fn forgejo_prune_keeps_absent_unscoped_history() {
    let fixture = Fixture::new("absent-unscoped-review");
    let storage = fixture.storage();
    let directory = fixture.repo.path().join("missing-worktree");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &directory,
        "phasegent/7-absent",
        "retained",
        None,
    );
    let mock = ZeroMock::start();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(exit, 0);
    assert!(requests.is_empty());
    assert_eq!(rows[0].status, "retained");
}

#[test]
fn explicit_legacy_acquire_overrides_stored_forgejo_default_without_auth() {
    for provider in ["local", "redmine"] {
        let fixture = Fixture::new(&format!("stored-default-{provider}-review"));
        let storage = Storage::open_at(&fixture.db.path().join("leases.sqlite3")).unwrap();
        ensure_schema(&storage).unwrap();
        let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        storage
            .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
            .unwrap();
        let mock = ZeroMock::start();
        let args: Vec<String> = [
            "--provider",
            provider,
            "worktree",
            "acquire",
            "--issue",
            "7",
            "--session",
            "fixture-session",
            "--no-sync",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        let requests = mock.finish();
        let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        assert_eq!(exit, 0, "explicit {provider} must override stored Forgejo");
        assert!(requests.is_empty());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "active");
        assert!(
            crate::worktree::lease_scope::scope_map_for_repo(&storage, &fixture.identity())
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn implicit_forgejo_defaults_reject_acquire_before_existing_taxi_candidate() {
    for default in ["env", "stored"] {
        let fixture = Fixture::new(&format!("implicit-taxi-{default}-review"));
        let storage = fixture.storage();
        let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        let _default = if default == "env" {
            Some(EnvGuard::set("PHASEGENT_PROVIDER", "forgejo"))
        } else {
            storage
                .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
                .unwrap();
            None
        };
        let directory = fixture.worktree("phasegent/7-implicit-taxi");
        fixture.lease(
            &storage,
            "legacy-seven",
            7,
            &directory,
            "phasegent/7-implicit-taxi",
            "retained",
            None,
        );
        let before = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        let mock = ZeroMock::start();
        let args: Vec<String> = [
            "worktree",
            "acquire",
            "--issue",
            "8",
            "--session",
            "fixture-session",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        let requests = mock.finish();
        let after = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        assert_ne!(exit, 0, "{default} default must fail before taxi");
        assert!(requests.is_empty());
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0].status, before[0].status);
        assert_eq!(after[0].heartbeat_at, before[0].heartbeat_at);
        assert!(directory.exists());
    }
}

#[cfg(unix)]
#[test]
fn legacy_closes_recheck_other_issue_before_removing_shared_directory() {
    let fixture = Fixture::new("legacy-close-sequential-review");
    let storage = fixture.storage();
    let local_path = fixture.db.path().join("explicit-local.sqlite3");
    let local = crate::infra::local_store::open_local_at(&local_path).unwrap();
    for id in [7, 9] {
        local
            .connection
            .execute(
                "INSERT INTO local_issues (id,title,body,status,project,tracker,author_role,created_at,updated_at,closed_at) VALUES (?1,'fixture','', 'Resolved','default','Task','executor',1,1,NULL)",
                [id],
            )
            .unwrap();
    }
    drop(local);
    let _local_env = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_path.to_string_lossy().as_ref(),
    );
    let directory = fixture.worktree("phasegent/7-close-sequential");
    for (id, status) in [(7, "active"), (9, "retained")] {
        fixture.lease(
            &storage,
            &format!("legacy-{id}"),
            id,
            &directory,
            "phasegent/7-close-sequential",
            status,
            None,
        );
    }
    let (_stub, _path) = opencode_stub();
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(9, "closed", "[]")]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let close = |number: &str| {
        let args: Vec<String> = ["--provider", "local", "issue", "close", number]
            .into_iter()
            .map(str::to_owned)
            .collect();
        run_as(fixture.repo.path(), "orchestrator", &args)
    };
    assert_eq!(close("7"), 0);
    assert!(directory.exists(), "other explicit issue is still open");
    assert_eq!(close("9"), 0);
    assert!(!directory.exists(), "both explicit issues are now closed");
    let requests = mock.finish();
    assert!(requests.is_empty(), "the default provider must not be read");
}

#[test]
fn rejected_env_default_leaves_absent_store_absent_across_entry_paths() {
    let fixture = Fixture::new("absent-default-admission-review");
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let path = fixture.db.path().join("leases.sqlite3");
    let commands: &[&[&str]] = &[
        &[
            "worktree",
            "acquire",
            "--issue",
            "7",
            "--session",
            "fixture-session",
            "--no-sync",
        ],
        &[
            "worktree",
            "acquire",
            "--issue",
            "7",
            "--session",
            "fixture-session",
        ],
        &["worktree", "list"],
        &[
            "worktree",
            "prune",
            "--release-stale",
            "--stale-days",
            "0",
            "--reason",
            "fixture",
        ],
    ];
    for command in commands {
        assert!(!path.exists());
        let args: Vec<String> = command.iter().map(|arg| (*arg).to_owned()).collect();
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_ne!(exit, 0, "{command:?}");
        assert!(
            stderr.contains("explicit --provider forgejo"),
            "{command:?} must report its own diagnostic: {stderr}"
        );
        assert!(!path.exists(), "{command:?} created the store");
    }
}

#[test]
fn rejected_stored_default_does_not_migrate_existing_store() {
    let fixture = Fixture::new("stored-default-no-migration-review");
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let path = fixture.db.path().join("leases.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); \
             INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER', 'forgejo');",
        )
        .unwrap();
    let count_tables = || -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let before = count_tables();
    let args: Vec<String> = [
        "worktree",
        "acquire",
        "--issue",
        "7",
        "--session",
        "fixture-session",
        "--no-sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let after = count_tables();
    assert_ne!(exit, 0, "implicit Forgejo default must be rejected");
    assert_eq!(
        after, before,
        "the rejected invocation must not initialize or migrate the existing store"
    );
}

#[cfg(unix)]
#[test]
fn forgejo_prune_without_credential_keeps_absent_legacy_history() {
    let fixture = Fixture::new("absent-no-credential-review");
    let storage = Storage::open_at(&fixture.db.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    let directory = fixture.repo.path().join("missing-worktree");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &directory,
        "phasegent/7-absent",
        "retained",
        None,
    );
    let mock = ZeroMock::start();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert!(requests.is_empty());
    assert_eq!(
        rows[0].status, "retained",
        "missing Forgejo credentials exited {exit} but changed legacy history"
    );
}

#[cfg(unix)]
#[test]
fn forgejo_prune_keeps_same_number_legacy_on_another_path() {
    let fixture = Fixture::new("prune-same-number-other-path-review");
    let storage = fixture.storage();
    let scoped = scoped_lease(&fixture, &storage, 7, "retained");
    let legacy = fixture.worktree("phasegent/7-legacy-other");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &legacy,
        "phasegent/7-legacy-other",
        "retained",
        None,
    );
    let (_stub, _path) = opencode_stub();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
    ]);
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert!(
        scoped.exists(),
        "prune exited {exit} and removed the scoped worktree despite same-number legacy history; requests: {requests:?}"
    );
    assert!(legacy.exists());
    assert!(requests.is_empty());
}

#[test]
fn sync_keeps_when_same_number_legacy_arrives_on_another_path_after_proof() {
    let fixture = Fixture::new("sync-other-path-post-proof-review");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let scoped = scoped_lease(&fixture, &storage, 7, "active");
    let legacy = fixture.worktree("phasegent/7-legacy-late");
    let identity = fixture.identity();
    let late_path = legacy.clone();
    crate::cli::issue::sync::set_retention_interleave_hook(Box::new(move |storage| {
        insert_lease(
            storage,
            NewLease {
                lease_id: "late-legacy",
                identity: &identity,
                issue: 7,
                session: "session-late",
                checkout_path: late_path.parent().unwrap().to_str().unwrap(),
                worktree_path: late_path.to_str().unwrap(),
                branch: "phasegent/7-legacy-late",
                status: "retained",
                created_at: 2,
                heartbeat_at: crate::worktree::now_unix_secs(),
            },
        )
        .unwrap();
    }));
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(7, "closed", "[]")]);
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "active",
        "post-proof same-number legacy arrival must invalidate retention; exit {exit}, requests {requests:?}"
    );
    assert!(scoped.exists());
    assert!(legacy.exists());
}

#[test]
fn malformed_stored_default_fails_closed_before_acquire() {
    let fixture = Fixture::new("malformed-stored-default-review");
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let path = fixture.db.path().join("leases.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); \
             INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER', X'FF');",
        )
        .unwrap();
    let args: Vec<String> = [
        "worktree",
        "acquire",
        "--issue",
        "7",
        "--session",
        "fixture-session",
        "--no-sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(exit, 0, "malformed persisted default must fail closed");
    assert_eq!(
        table_count, 1,
        "a rejected invocation must not migrate the store"
    );
}

#[test]
fn old_role_config_with_missing_required_columns_is_absent_for_admission() {
    let fixture = Fixture::new("old-role-columns-review");
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let path = fixture.db.path().join("leases.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE role_config (role TEXT PRIMARY KEY, provider TEXT); \
             INSERT INTO role_config VALUES ('orchestrator', 'local');",
        )
        .unwrap();
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None);
    assert_eq!(
        resolved.unwrap(),
        crate::providers::ProviderKind::Redmine,
        "a persisted step missing required old-schema columns is absent"
    );
}

#[test]
fn prune_absent_scoped_lease_keeps_when_same_number_arrives_during_scan() {
    use crate::cli::issue_closure::{IssueClosure, IssueStateSource};
    use crate::worktree::leases::StaleRecoveryScope;
    use std::cell::Cell;
    struct Arrival<'a> {
        storage: &'a Storage,
        identity: String,
        path: PathBuf,
        inserted: Cell<bool>,
    }
    impl IssueStateSource for Arrival<'_> {
        fn provider_scope(&self) -> Option<(String, String)> {
            Some(("forgejo".to_owned(), "owner/repo".to_owned()))
        }
        fn closure(&self, _: &Path, _: u64) -> IssueClosure {
            if !self.inserted.replace(true) {
                insert_lease(
                    self.storage,
                    NewLease {
                        lease_id: "late-legacy-seven",
                        identity: &self.identity,
                        issue: 7,
                        session: "late-legacy",
                        checkout_path: self.path.parent().unwrap().to_str().unwrap(),
                        worktree_path: self.path.to_str().unwrap(),
                        branch: "phasegent/7-late",
                        status: "retained",
                        created_at: 2,
                        heartbeat_at: 2,
                    },
                )
                .unwrap();
            }
            IssueClosure::Keep("fixture other issue stays open".to_owned())
        }
    }
    let fixture = Fixture::new("prune-absent-scope-race-review");
    let storage = fixture.storage();
    let missing = fixture.repo.path().join("a-missing");
    let seven = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    fixture.lease(
        &storage,
        "scoped-seven",
        7,
        &missing,
        "phasegent/7-missing",
        "retained",
        Some(&seven),
    );
    let existing = fixture.worktree("phasegent/8-existing");
    let eight = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &existing,
        "phasegent/8-existing",
        "retained",
        Some(&eight),
    );
    let source = Arrival {
        storage: &storage,
        identity: fixture.identity(),
        path: fixture.repo.path().join("z-late"),
        inserted: Cell::new(false),
    };
    let api = crate::worktree::opencode::test_support::FakeOpenCodeApi::empty();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let exit = crate::cli::worktree::execute_prune_with_source(
        Some(&repo),
        7,
        false,
        None,
        false,
        "json",
        &source,
        &api,
        &|_| {},
        StaleRecoveryScope::Exact {
            provider: "forgejo",
            project: "owner/repo",
        },
        true,
    );
    assert_eq!(exit, 0);
    assert!(source.inserted.get(), "fixture must insert during the scan");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-seven")
            .unwrap()
            .status,
        "retained",
        "late same-number legacy evidence must keep the absent scoped lease"
    );
}

#[cfg(unix)]
#[test]
fn unreadable_existing_store_fails_closed_during_admission() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("unreadable-store-review");
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let blocked = fixture.db.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    let path = blocked.join("leases.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); \
             INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER', 'forgejo');",
        )
        .unwrap();
    drop(connection);
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", path.to_str().unwrap());
    let original = std::fs::metadata(&blocked).unwrap().permissions();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let metadata_error = std::fs::metadata(&path).err();
    let result = crate::providers::config::resolve_kind(Role::Orchestrator, None);
    let args: Vec<String> = [
        "worktree",
        "acquire",
        "--issue",
        "7",
        "--session",
        "fixture-session",
        "--no-sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let observed_mode = std::fs::metadata(&blocked).unwrap().permissions().mode() & 0o777;
    std::fs::set_permissions(&blocked, original).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let leases: Option<i64> = connection
        .query_row("SELECT COUNT(*) FROM worktree_leases", [], |row| row.get(0))
        .ok();
    if metadata_error.is_none() {
        return;
    }
    assert_eq!(
        metadata_error.unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(
        result.is_err(),
        "an existing unreadable store must not be treated as absent: {result:?}; acquire exit={exit}, mode={observed_mode:o}, leases={leases:?}"
    );
    assert_ne!(exit, 0, "unreadable evidence must block acquisition");
}

#[test]
fn absent_release_keeps_when_different_issue_legacy_joins_same_path() {
    let fixture = Fixture::new("absent-mixed-arrival-review");
    let storage = fixture.storage();
    let path = fixture.repo.path().join("missing-worktree");
    let scope = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    fixture.lease(
        &storage,
        "scoped-seven",
        7,
        &path,
        "phasegent/7-absent",
        "retained",
        Some(&scope),
    );
    let targets = vec![(
        "scoped-seven".to_owned(),
        "retained".to_owned(),
        path.to_str().unwrap().to_owned(),
    )];
    let canonical = crate::worktree::CanonicalPaths::default().resolve(&path);
    let approved_rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let approved_scopes =
        crate::worktree::lease_scope::scope_map_for_repo(&storage, &fixture.identity()).unwrap();
    let approved = crate::worktree::leases::directory_ownership_proof(
        &canonical,
        &approved_rows,
        &approved_scopes,
    );
    fixture.lease(
        &storage,
        "late-legacy-eight",
        8,
        &path,
        "phasegent/8-late",
        "retained",
        None,
    );
    let result = crate::worktree::leases::release_absent_leases_proven(
        &storage,
        &fixture.identity(),
        &targets,
        &approved,
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-seven")
            .unwrap()
            .status,
        "retained",
        "new mixed ownership must abort absent release: {result:?}"
    );
}

#[test]
fn forgejo_create_records_only_a_scoped_lease() {
    let fixture = Fixture::new("create");
    let storage = fixture.storage();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        LABELS_JSON.to_owned(),
        "[]".to_owned(),
        issue_json(7, "open", r#"[{"id":1,"name":"status/New"}]"#),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--output",
        "json",
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "create",
        "--title",
        "fixture",
        "--body",
        "fixture",
        "--session",
        "session-create",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = in_dir(fixture.repo.path(), || {
        crate::cli::run_with_role(args, Some("orchestrator"))
    });
    assert_eq!(exit, 0, "issue create must succeed: {}", mock.base);
    let requests = mock.finish();
    assert!(
        requests.iter().any(|request| request.starts_with("POST ")),
        "create must reach the provider"
    );

    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let for_seven: Vec<_> = rows.iter().filter(|row| row.issue == 7).collect();
    assert_eq!(
        for_seven.len(),
        1,
        "exactly one lease for the created issue"
    );
    assert_eq!(for_seven[0].status, "active");
    let scopes =
        crate::worktree::lease_scope::scope_map_for_repo(&storage, &fixture.identity()).unwrap();
    let scope = scopes
        .get(&for_seven[0].lease_id)
        .expect("the created lease must carry a sidecar scope");
    assert_eq!(scope.provider(), "forgejo");
    assert_eq!(scope.project(), "owner/repo");
    assert_eq!(scope.external_id(), 7);
}

#[test]
fn forgejo_close_keeps_active_lease_with_mismatched_sidecar_number() {
    let fixture = Fixture::new("close-corrupt-sidecar-review");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let storage = fixture.storage();
    let scope = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    let directory = fixture.worktree("phasegent/8-corrupt");
    fixture.lease(
        &storage,
        "scoped-eight-corrupt",
        8,
        &directory,
        "phasegent/8-corrupt",
        "active",
        Some(&scope),
    );
    storage
        .connection
        .execute(
            "UPDATE worktree_lease_scopes SET external_id = 7 WHERE lease_id = 'scoped-eight-corrupt'",
            [],
        )
        .unwrap();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "open", r#"[{"id":4,"name":"status/Resolved"}]"#),
        LABELS_JSON.to_owned(),
        "[]".to_owned(),
        "[]".to_owned(),
        issue_json(7, "open", r#"[{"id":4,"name":"status/Resolved"}]"#),
        issue_json(7, "closed", r#"[{"id":4,"name":"status/Resolved"}]"#),
        "[]".to_owned(),
    ]);
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "issue",
        "close",
        "7",
        "--worktree-session",
        "session-fixture",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(exit, 0, "fixture close: {requests:?}");
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-eight-corrupt")
            .unwrap()
            .status,
        "active",
        "closing 7 must not retain the issue 8 lease when its sidecar is corrupt"
    );
    assert!(directory.exists());
}

#[test]
fn forgejo_close_aborts_all_scope_release_on_corrupt_map_and_closes_normally() {
    let snapshot = |storage: &Storage, identity: &str| -> Vec<String> {
        let mut statement = storage
            .connection
            .prepare(
                "SELECT l.lease_id, l.issue, l.status, l.worktree_path, l.session, l.branch, \
                        l.heartbeat_at, COALESCE(l.release_reason,''), \
                        COALESCE(s.provider,''), COALESCE(s.project,''), COALESCE(s.external_id,-1) \
                   FROM worktree_leases l \
                   LEFT JOIN worktree_lease_scopes s ON s.lease_id = l.lease_id \
                  WHERE l.repo_identity = ?1 ORDER BY l.lease_id, l.worktree_path",
            )
            .unwrap();
        statement
            .query_map([identity], |row| {
                Ok(format!(
                    "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            })
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    let close_args = |base: &str| -> Vec<String> {
        [
            "--provider",
            "forgejo",
            "--api-base",
            base,
            "--repository",
            "owner/repo",
            "issue",
            "close",
            "7",
            "--worktree-session",
            "session-fixture",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    };
    let responses = || {
        vec![
            REPO_JSON.to_owned(),
            issue_json(7, "open", r#"[{"id":4,"name":"status/Resolved"}]"#),
            LABELS_JSON.to_owned(),
            "[]".to_owned(),
            "[]".to_owned(),
            issue_json(7, "open", r#"[{"id":4,"name":"status/Resolved"}]"#),
            issue_json(7, "closed", r#"[{"id":4,"name":"status/Resolved"}]"#),
            "[]".to_owned(),
        ]
    };

    let corrupt = Fixture::new("close-corrupt-all-or-nothing");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        corrupt.db.path().join("missing.toml").to_str().unwrap(),
    );
    let storage = corrupt.storage();
    let valid_dir = corrupt.worktree("phasegent/7-valid");
    let invalid_dir = corrupt.worktree("phasegent/8-invalid");
    corrupt.lease(
        &storage,
        "scoped-seven-valid",
        7,
        &valid_dir,
        "phasegent/7-valid",
        "active",
        Some(&LeaseScope::new("forgejo", "owner/repo", 7).unwrap()),
    );
    corrupt.lease(
        &storage,
        "scoped-eight-invalid",
        8,
        &invalid_dir,
        "phasegent/8-invalid",
        "active",
        Some(&LeaseScope::new("forgejo", "owner/repo", 8).unwrap()),
    );
    storage
        .connection
        .execute(
            "UPDATE worktree_lease_scopes SET external_id = 7 WHERE lease_id = 'scoped-eight-invalid'",
            [],
        )
        .unwrap();
    let before = snapshot(&storage, &corrupt.identity());
    let lease_snapshot = |storage: &Storage| {
        complete_fixture_snapshot(&storage.connection)
            .into_iter()
            .filter(|(name, _)| name == "worktree_leases" || name == "worktree_lease_scopes")
            .collect::<Vec<_>>()
    };
    let full_before = lease_snapshot(&storage);
    let mock = Mock::start(responses());
    let exit = run_as(corrupt.repo.path(), "orchestrator", &close_args(&mock.base));
    let _requests = mock.finish();
    assert_eq!(exit, 0, "the corrupt-map close still completes remotely");
    assert_eq!(
        snapshot(&storage, &corrupt.identity()),
        before,
        "a corrupt scope map must abort every scoped release update"
    );
    assert_eq!(
        lease_snapshot(&storage),
        full_before,
        "a corrupt map must preserve every lease and sidecar column"
    );
    assert!(valid_dir.exists() && invalid_dir.exists());
    drop(storage);
    drop(corrupt);

    let normal = Fixture::new("close-valid-scope-normal");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        normal.db.path().join("missing.toml").to_str().unwrap(),
    );
    let storage = normal.storage();
    let directory = normal.worktree("phasegent/7-normal");
    normal.lease(
        &storage,
        "scoped-seven-normal",
        7,
        &directory,
        "phasegent/7-normal",
        "active",
        Some(&LeaseScope::new("forgejo", "owner/repo", 7).unwrap()),
    );
    let before = snapshot(&storage, &normal.identity());
    let mock = Mock::start(responses());
    let exit = run_as(normal.repo.path(), "orchestrator", &close_args(&mock.base));
    let _requests = mock.finish();
    assert_eq!(exit, 0, "a valid exact-scope close succeeds");
    let rows = list_all_for_repo(&storage, &normal.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-seven-normal")
            .unwrap()
            .status,
        "retained",
        "a valid exact-scope close retains its own lease"
    );
    let scope_provider: String = storage
        .connection
        .query_row(
            "SELECT provider || '/' || project || '/' || external_id FROM worktree_lease_scopes WHERE lease_id = 'scoped-seven-normal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(scope_provider, "forgejo/owner/repo/7");
    assert_eq!(before.len(), 1);
}

#[test]
fn forgejo_close_releases_only_its_exact_scope() {
    let fixture = Fixture::new("close");
    let storage = fixture.storage();
    let scoped_seven = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    let scoped_eight = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    let legacy_dir = fixture.worktree("phasegent/7-legacy");
    let scoped_dir = fixture.worktree("phasegent/7-forgejo");
    let other_dir = fixture.worktree("phasegent/8-other");
    // The scoped row is inserted first: production never creates a legacy and
    // a scoped lease for one number, but an older repository can still hold a
    // legacy row alongside a later Forgejo task.
    fixture.lease(
        &storage,
        "scoped-seven",
        7,
        &scoped_dir,
        "phasegent/7-forgejo",
        "active",
        Some(&scoped_seven),
    );
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &other_dir,
        "phasegent/8-other",
        "active",
        Some(&scoped_eight),
    );
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &legacy_dir,
        "phasegent/7-legacy",
        "active",
        None,
    );

    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "open", r#"[{"id":4,"name":"status/Resolved"}]"#),
        LABELS_JSON.to_owned(),
        "[]".to_owned(),
        "[]".to_owned(),
        issue_json(7, "open", r#"[{"id":4,"name":"status/Resolved"}]"#),
        issue_json(7, "closed", r#"[{"id":4,"name":"status/Resolved"}]"#),
        "[]".to_owned(),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--output",
        "json",
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "close",
        "7",
        "--worktree-session",
        "session-fixture",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = in_dir(fixture.repo.path(), || {
        crate::cli::run_with_role(args, Some("orchestrator"))
    });
    let requests = mock.finish();
    assert_eq!(exit, 0, "issue close must succeed; requests: {requests:?}");
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("/issues/8")),
        "the close must never address the other scoped task: {requests:?}"
    );

    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let status = |id: &str| {
        rows.iter()
            .find(|row| row.lease_id == id)
            .unwrap()
            .status
            .clone()
    };
    assert_eq!(
        status("scoped-seven"),
        "retained",
        "the closed issue's exact scope is released"
    );
    assert_eq!(
        status("legacy-seven"),
        "active",
        "a same-number legacy row is never released by a Forgejo close"
    );
    assert_eq!(
        status("scoped-eight"),
        "active",
        "another scoped task is untouched"
    );
    assert!(
        !scoped_dir.exists(),
        "the matching-scope eligible worktree is cleaned"
    );
    assert!(
        legacy_dir.exists(),
        "the same-number legacy worktree is never cleaned"
    );
    assert!(
        other_dir.exists(),
        "the other scoped task's worktree is never cleaned"
    );
}

/// A counting endpoint that records any connection while the command runs.
struct ZeroMock {
    base: String,
    task: JoinHandle<Vec<String>>,
}

impl ZeroMock {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let mut requests = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut data = Vec::new();
                        let _ = stream.read_to_end(&mut data);
                        requests.push(String::from_utf8_lossy(&data).into_owned());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            requests
        });
        Self { base, task }
    }

    fn finish(self) -> Vec<String> {
        self.task.join().unwrap()
    }
}

fn run_as(repo: &Path, role: &str, args: &[String]) -> i32 {
    in_dir(repo, || {
        crate::cli::run_with_role(args.to_vec(), Some(role))
    })
}

fn cli_binary() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_phasegent") {
        return PathBuf::from(path);
    }
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("phasegent{}", std::env::consts::EXE_SUFFIX))
}

fn run_captured(repo: &Path, args: &[String]) -> (i32, String, String) {
    let output = std::process::Command::new(cli_binary())
        .args(args)
        .current_dir(repo)
        .env("PHASEGENT_ROLE", "orchestrator")
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn assert_rejected(repo: &Path, role: &str, args: &[String], mock: ZeroMock) {
    let exit = run_as(repo, role, args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "the command must be rejected");
    assert!(
        requests.is_empty(),
        "the guard must reject before any HTTP request: {requests:?}"
    );
}

#[test]
fn forgejo_env_default_without_the_flag_is_rejected_before_http() {
    let fixture = Fixture::new("env-default");
    fixture.storage();
    let _env = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let mock = ZeroMock::start();
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "get",
        "7",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_rejected(fixture.repo.path(), "orchestrator", &args, mock);
}

#[test]
fn forgejo_stored_default_without_the_flag_is_rejected_before_http() {
    let fixture = Fixture::new("stored-default");
    let storage = fixture.storage();
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
        .unwrap();
    let mock = ZeroMock::start();
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "get",
        "7",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_rejected(fixture.repo.path(), "orchestrator", &args, mock);
}

#[test]
fn omitted_provider_on_a_forgejo_bound_number_is_rejected_before_http() {
    let fixture = Fixture::new("omitted");
    fixture.link(7, "owner/repo");
    let _env = EnvGuard::set("PHASEGENT_PROVIDER", "local");
    let args: Vec<String> = ["issue", "get", "7"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_rejected(
        fixture.repo.path(),
        "orchestrator",
        &args,
        ZeroMock::start(),
    );
}

#[test]
fn get_batch_rejects_a_forgejo_bound_number_before_http() {
    let fixture = Fixture::new("getbatch");
    fixture.link(7, "owner/repo");
    let _env = EnvGuard::set("PHASEGENT_PROVIDER", "local");
    let args: Vec<String> = ["issue", "get", "7", "8"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_rejected(
        fixture.repo.path(),
        "orchestrator",
        &args,
        ZeroMock::start(),
    );
}

#[test]
fn matching_explicit_provider_updates_a_forgejo_bound_number() {
    let fixture = Fixture::new("matching");
    fixture.storage();
    fixture.link(7, "owner/repo");
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "open", "[]"),
        issue_json(7, "open", "[]"),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "update",
        "7",
        "--body",
        "updated",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(
        exit, 0,
        "matching explicit scope must proceed: {requests:?}"
    );
    assert!(
        requests.iter().any(|request| request.starts_with("PATCH ")),
        "the matching update must reach Forgejo: {requests:?}"
    );
}

#[test]
fn conflicting_repository_is_rejected_before_http() {
    let fixture = Fixture::new("conflict");
    fixture.storage();
    fixture.link(7, "owner/repo");
    let mock = ZeroMock::start();
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "other/repo",
        "issue",
        "get",
        "7",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_rejected(fixture.repo.path(), "orchestrator", &args, mock);
}

#[test]
fn record_create_rejects_an_omitted_provider_on_a_forgejo_bound_number() {
    let fixture = Fixture::new("record-omitted");
    fixture.storage();
    fixture.link(7, "owner/repo");
    let _env = EnvGuard::set("PHASEGENT_PROVIDER", "local");
    let args: Vec<String> = [
        "record",
        "create",
        "7",
        "--kind",
        "executor",
        "--key",
        "k",
        "--phase",
        "P1",
        "--attempt",
        "1",
        "--body",
        "note",
        "--authorized",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_rejected(fixture.repo.path(), "executor", &args, ZeroMock::start());
}

fn status_transition_responses(current_label: &str, target_label: &str) -> Vec<String> {
    vec![
        REPO_JSON.to_owned(),
        issue_json(7, "open", current_label),
        LABELS_JSON.to_owned(),
        "[]".to_owned(),
        issue_json(7, "open", current_label),
        issue_json(7, "open", current_label),
        target_label.to_owned(),
    ]
}

#[test]
fn forgejo_status_next_matrix() {
    let fixture = Fixture::new("status-next");
    fixture.storage();
    fixture.link(7, "owner/repo");
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "open", r#"[{"id":2,"name":"status/In Progress"}]"#),
        LABELS_JSON.to_owned(),
        "[]".to_owned(),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "status",
        "next",
        "7",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "status next must succeed: {requests:?}");
}

#[test]
fn forgejo_status_set_matrix() {
    let fixture = Fixture::new("status-set");
    fixture.storage();
    fixture.link(7, "owner/repo");
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        LABELS_JSON.to_owned(),
        "[]".to_owned(),
        issue_json(7, "open", r#"[{"id":1,"name":"status/New"}]"#),
        LABELS_JSON.to_owned(),
        "[]".to_owned(),
        issue_json(7, "open", r#"[{"id":1,"name":"status/New"}]"#),
        issue_json(7, "open", r#"[{"id":1,"name":"status/New"}]"#),
        r#"[{"id":2,"name":"status/In Progress"}]"#.to_owned(),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "status",
        "set",
        "7",
        "--status",
        "In Progress",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "status set must succeed: {requests:?}");
    assert!(
        requests.iter().any(|request| request.starts_with("PATCH ")),
        "status set must patch the native state: {requests:?}"
    );
}

#[test]
fn forgejo_status_advance_matrix() {
    let fixture = Fixture::new("status-advance");
    fixture.storage();
    fixture.link(7, "owner/repo");
    let mock = Mock::start(status_transition_responses(
        r#"[{"id":3,"name":"status/In Review"}]"#,
        r#"[{"id":4,"name":"status/Resolved"}]"#,
    ));
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "status",
        "advance",
        "7",
        "--status",
        "Resolved",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "status advance must succeed: {requests:?}");
    assert!(
        requests.iter().any(|request| request.starts_with("PUT ")),
        "status advance must update labels: {requests:?}"
    );
}

#[test]
fn same_repository_multi_issue_history_cleans_up_once_all_are_closed() {
    let fixture = Fixture::new("shared-directory");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    fixture.link(8, "owner/repo");
    let seven = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    let eight = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    let shared = fixture.worktree("phasegent/7-shared");
    fixture.lease(
        &storage,
        "scoped-seven",
        7,
        &shared,
        "phasegent/7-shared",
        "retained",
        Some(&seven),
    );
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &shared,
        "phasegent/7-shared",
        "active",
        Some(&eight),
    );

    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(8, "open", r#"[{"id":4,"name":"status/Resolved"}]"#),
        LABELS_JSON.to_owned(),
        "[]".to_owned(),
        "[]".to_owned(),
        issue_json(8, "open", r#"[{"id":4,"name":"status/Resolved"}]"#),
        issue_json(8, "closed", r#"[{"id":4,"name":"status/Resolved"}]"#),
        "[]".to_owned(),
        REPO_JSON.to_owned(),
        issue_json(7, "closed", r#"[{"id":4,"name":"status/Resolved"}]"#),
        issue_json(7, "closed", r#"[{"id":4,"name":"status/Resolved"}]"#),
        issue_json(7, "closed", r#"[{"id":4,"name":"status/Resolved"}]"#),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "close",
        "8",
        "--worktree-session",
        "session-fixture",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "the close must succeed: {requests:?}");

    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let status = |id: &str| {
        rows.iter()
            .find(|row| row.lease_id == id)
            .unwrap()
            .status
            .clone()
    };
    assert_eq!(
        status("scoped-seven"),
        "retained",
        "the earlier same-repository lease stays retained"
    );
    assert_eq!(
        status("scoped-eight"),
        "retained",
        "only the closing issue's exact lease is released"
    );
    assert!(
        !shared.exists(),
        "both closed same-repository issues allow the shared directory cleanup"
    );
}

#[test]
fn omitted_provider_on_a_forgejo_bound_status_route_is_rejected_before_http() {
    let fixture = Fixture::new("status-omitted");
    fixture.link(7, "owner/repo");
    let _env = EnvGuard::set("PHASEGENT_PROVIDER", "local");
    let args: Vec<String> = ["status", "next", "7"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_rejected(
        fixture.repo.path(),
        "orchestrator",
        &args,
        ZeroMock::start(),
    );
}

fn scoped_lease(fixture: &Fixture, storage: &Storage, issue: u64, status: &str) -> PathBuf {
    let scope = LeaseScope::new("forgejo", "owner/repo", issue).unwrap();
    let directory = fixture.worktree(&format!("phasegent/{issue}-sync"));
    fixture.lease(
        storage,
        &format!("scoped-{issue}"),
        issue,
        &directory,
        &format!("phasegent/{issue}-sync"),
        status,
        Some(&scope),
    );
    directory
}

#[test]
fn scoped_sync_mixed_directory_keeps_active_lease() {
    let fixture = Fixture::new("sync-mixed-review");
    let storage = fixture.storage();
    fixture.link(8, "owner/repo");
    let directory = fixture.worktree("phasegent/8-mixed");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &directory,
        "phasegent/8-mixed",
        "retained",
        None,
    );
    let scope = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &directory,
        "phasegent/8-mixed",
        "active",
        Some(&scope),
    );
    let mock = ZeroMock::start();
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "mixed scope must be kept: {requests:?}");
    assert!(
        requests.is_empty(),
        "mixed evidence must keep before any provider request: {requests:?}"
    );
    assert!(directory.exists());
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-eight")
            .unwrap()
            .status,
        "active",
        "mixed directory must not release a lease"
    );
}

#[test]
fn scoped_sync_no_clean_reports_without_releasing_or_removing() {
    let fixture = Fixture::new("sync-report");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(7, "closed", "[]")]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
        "--no-clean",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "report mode must succeed: {requests:?}");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "active",
        "--no-clean must not release the scoped lease"
    );
    assert!(
        directory.exists(),
        "--no-clean must not remove the directory"
    );
    assert!(
        requests.iter().all(|request| request.starts_with("GET ")),
        "--no-clean must not write to the provider: {requests:?}"
    );
}

#[test]
fn scoped_sync_open_issue_leaves_the_lease_active() {
    let fixture = Fixture::new("sync-open");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(7, "open", "[]")]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "an open issue is not an error: {requests:?}");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "active"
    );
    assert!(directory.exists());
}

#[test]
fn explicit_forgejo_sync_does_not_reconcile_unscoped_legacy_issue() {
    let fixture = Fixture::new("sync-legacy-isolation-review");
    let storage = fixture.storage();
    let directory = fixture.worktree("phasegent/7-legacy-review");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &directory,
        "phasegent/7-legacy-review",
        "active",
        None,
    );
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(7, "closed", "[]")]);
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows[0].status, "active",
        "Forgejo sync exited {exit} and changed an unscoped lease; requests: {requests:?}"
    );
    assert!(directory.exists(), "legacy directory must remain");
    assert!(requests.is_empty(), "unknown scope must keep before HTTP");
}

#[test]
fn scoped_sync_provider_error_leaves_the_lease_active() {
    let fixture = Fixture::new("sync-error");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    let mock = ZeroMock::start();
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "a provider read failure must fail the pass");
    assert_eq!(requests.len(), 1, "only the failed read was attempted");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "active"
    );
    assert!(directory.exists());
}

#[test]
fn scoped_sync_closed_matching_issue_cleans_up() {
    let fixture = Fixture::new("sync-clean");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(7, "closed", "[]")]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "clean sync must succeed: {requests:?}");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "retained"
    );
    assert!(!directory.exists(), "the eligible directory is removed");
    assert!(
        requests.iter().all(|request| request.starts_with("GET ")),
        "sync cleanup must not write to the provider: {requests:?}"
    );
}

#[cfg(unix)]
fn opencode_stub() -> (PathBuf, EnvGuard) {
    use std::os::unix::fs::PermissionsExt;
    let dir = crate::test_scratch::root().join(format!(
        "phasegent-opencode-stub-{}-{}",
        std::process::id(),
        crate::worktree::now_unix_secs()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("opencode");
    std::fs::write(
        &program,
        "#!/bin/sh\nif [ \"$2\" = \"session.active\" ]; then\n  echo '{\"data\":{}}'\nelse\n  echo '{\"data\":[],\"cursor\":{\"next\":null}}'\nfi\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let env = EnvGuard::set("PATH", &format!("{}:{path}", dir.display()));
    (dir, env)
}

#[cfg(unix)]
#[test]
fn implicit_forgejo_prune_does_not_recover_legacy_lease() {
    let fixture = Fixture::new("prune-implicit-recovery-review");
    let storage = fixture.storage();
    let directory = fixture.worktree("phasegent/7-legacy-stale");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &directory,
        "phasegent/7-legacy-stale",
        "active",
        None,
    );
    storage
        .connection
        .execute(
            "UPDATE worktree_leases SET heartbeat_at = 1 WHERE lease_id = 'legacy-seven'",
            [],
        )
        .unwrap();
    let (_stub, _path) = opencode_stub();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--release-stale",
        "--stale-days",
        "0",
        "--reason",
        "fixture recovery",
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert!(requests.is_empty());
    assert_eq!(
        rows[0].status, "active",
        "implicit Forgejo default exited {exit} and changed legacy stale ownership"
    );
    assert!(directory.exists());
}

#[cfg(unix)]
#[test]
fn explicit_forgejo_prune_does_not_remove_unscoped_legacy_issue() {
    let fixture = Fixture::new("prune-legacy-isolation-review");
    let storage = fixture.storage();
    let directory = fixture.worktree("phasegent/7-legacy-prune");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &directory,
        "phasegent/7-legacy-prune",
        "retained",
        None,
    );
    let (_stub, _path) = opencode_stub();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
    ]);
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert!(
        directory.exists(),
        "Forgejo prune exited {exit} and removed an unscoped directory; requests: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(rows[0].status, "retained");
    assert!(requests.is_empty(), "unknown scope must keep before HTTP");
}

#[cfg(unix)]
#[test]
fn production_prune_with_explicit_forgejo_context_cleans_the_scoped_worktree() {
    let fixture = Fixture::new("prune-explicit");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
    ]);
    let base = mock.base.clone();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "prune must succeed: {requests:?}");
    assert!(
        !directory.exists(),
        "the matching explicit Forgejo context must clean the scoped worktree: {requests:?}"
    );
}

#[cfg(unix)]
#[test]
fn production_prune_foreign_and_default_contexts_keep_the_scoped_worktree() {
    for (label, extra) in [
        (
            "foreign",
            vec!["--provider", "forgejo", "--repository", "other/repo"],
        ),
        ("default", vec![]),
    ] {
        let fixture = Fixture::new(&format!("prune-{label}"));
        let storage = fixture.storage();
        fixture.link(7, "owner/repo");
        let directory = scoped_lease(&fixture, &storage, 7, "retained");
        let (_stub, _path) = opencode_stub();
        let mock = ZeroMock::start();
        let base = mock.base.clone();
        let repo = fixture.repo.path().to_string_lossy().to_string();
        let mut args: Vec<String> = extra.iter().map(|value| (*value).to_owned()).collect();
        if label == "foreign" {
            args.extend(["--api-base".to_owned(), base]);
        }
        args.extend(
            ["worktree", "prune", "--repo", &repo, "--format", "json"]
                .into_iter()
                .map(str::to_owned),
        );
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        let requests = mock.finish();
        assert_eq!(exit, 0, "{label} prune must not fail");
        assert!(
            directory.exists(),
            "a {label} context must keep the scoped worktree"
        );
        assert!(
            requests.is_empty(),
            "a {label} context must not read the provider: {requests:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn production_prune_explicit_local_never_uses_forgejo_default() {
    let fixture = Fixture::new("prune-local-fg-default-review");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
    ]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "local",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0);
    assert!(
        requests.is_empty(),
        "explicit Local must not contact Forgejo: {requests:?}"
    );
    assert!(
        directory.exists(),
        "explicit Local must keep a Forgejo worktree"
    );
}

#[cfg(unix)]
#[test]
fn production_prune_rejects_incompatible_forgejo_project_override() {
    let fixture = Fixture::new("prune-fg-project-review");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
    ]);
    let base = mock.base.clone();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "--project-id",
        "23",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "Forgejo must reject --project-id");
    assert!(
        requests.is_empty(),
        "incompatible flags must fail before HTTP"
    );
    assert!(
        directory.exists(),
        "incompatible flags must not remove a worktree"
    );
}

#[cfg(unix)]
#[test]
fn production_prune_stale_recovery_uses_owned_forgejo_repository() {
    let fixture = Fixture::new("prune-owned-repo-review");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    storage
        .connection
        .execute(
            "UPDATE worktree_leases SET heartbeat_at = 1 WHERE lease_id = 'scoped-7'",
            [],
        )
        .unwrap();
    let (_stub, _path) = opencode_stub();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--release-stale",
        "--stale-days",
        "0",
        "--reason",
        "fixture recovery",
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "scoped recovery must succeed: {requests:?}");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "retained",
        "owned repository configuration must authorize exact-scope stale recovery"
    );
    assert!(
        directory.exists(),
        "the invocation-start active directory stays pinned"
    );
}

#[test]
fn production_sync_never_uses_implicit_forgejo_default() {
    let fixture = Fixture::new("sync-fg-default-review");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(7, "closed", "[]")]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let args: Vec<String> = ["issue", "sync"].into_iter().map(str::to_owned).collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "implicit Forgejo selection must be rejected");
    assert!(requests.is_empty(), "implicit Forgejo must not send HTTP");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "active"
    );
    assert!(directory.exists());
}

#[test]
fn scoped_sync_not_found_leaves_the_lease_active() {
    let fixture = Fixture::new("sync-notfound");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    let mock = Mock::start_status(vec![
        (200, REPO_JSON.to_owned()),
        (404, r#"{"message":"not found"}"#.to_owned()),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(
        exit, 0,
        "a 404 is an unproven state and must fail closed: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "active"
    );
    assert!(directory.exists());
}

#[test]
fn scoped_sync_multiissue_all_closed_cleans_shared_directory() {
    let fixture = Fixture::new("sync-shared-closed");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    fixture.link(8, "owner/repo");
    let directory = fixture.worktree("phasegent/8-shared-sync");
    let seven = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    let eight = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    fixture.lease(
        &storage,
        "scoped-seven",
        7,
        &directory,
        "phasegent/8-shared-sync",
        "retained",
        Some(&seven),
    );
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &directory,
        "phasegent/8-shared-sync",
        "active",
        Some(&eight),
    );
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "closed", "[]"),
        issue_json(8, "closed", "[]"),
        issue_json(8, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "all-closed shared sync must succeed: {requests:?}");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    for id in ["scoped-seven", "scoped-eight"] {
        assert_eq!(
            rows.iter().find(|row| row.lease_id == id).unwrap().status,
            "retained"
        );
    }
    assert!(
        !directory.exists(),
        "both closed issues allow the shared cleanup: {requests:?}"
    );
}

#[test]
fn scoped_sync_multiissue_open_keeps_shared_directory() {
    let fixture = Fixture::new("sync-shared-open");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    fixture.link(8, "owner/repo");
    let directory = fixture.worktree("phasegent/8-shared-open");
    let seven = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    let eight = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    fixture.lease(
        &storage,
        "scoped-seven",
        7,
        &directory,
        "phasegent/8-shared-open",
        "retained",
        Some(&seven),
    );
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &directory,
        "phasegent/8-shared-open",
        "active",
        Some(&eight),
    );
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "closed", "[]"),
        issue_json(8, "open", "[]"),
        issue_json(8, "open", "[]"),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "open shared sync must report: {requests:?}");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-eight")
            .unwrap()
            .status,
        "active"
    );
    assert!(directory.exists());
}

/// A corrupt sidecar scope map must skip the repository before the
/// `--release-stale` recovery or any scan mutation: leases and heartbeats stay
/// unchanged and no provider request is made in either preview or clean mode.
#[test]
fn production_prune_corrupt_scope_map_skips_before_recovery() {
    for (label, dry_run) in [("preview", true), ("release-stale", false)] {
        let fixture = Fixture::new(&format!("prune-corrupt-{label}"));
        let storage = fixture.storage();
        fixture.link(7, "owner/repo");
        let directory = scoped_lease(&fixture, &storage, 7, "active");
        storage
            .connection
            .execute(
                "UPDATE worktree_leases SET heartbeat_at = 1 WHERE lease_id = 'scoped-7'",
                [],
            )
            .unwrap();
        storage
            .connection
            .execute(
                "UPDATE worktree_lease_scopes SET external_id = 999 WHERE lease_id = 'scoped-7'",
                [],
            )
            .unwrap();
        let (_stub, _path) = opencode_stub();
        let mock = ZeroMock::start();
        let base = mock.base.clone();
        let repo = fixture.repo.path().to_string_lossy().to_string();
        let mut args: Vec<String> = [
            "--provider",
            "forgejo",
            "--api-base",
            &base,
            "--repository",
            "owner/repo",
            "worktree",
            "prune",
            "--repo",
            &repo,
            "--format",
            "json",
            "--release-stale",
            "--stale-days",
            "0",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if dry_run {
            args.push("--dryrun".to_owned());
        } else {
            args.extend(["--reason".to_owned(), "fixture recovery".to_owned()]);
        }
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        let requests = mock.finish();
        assert_eq!(exit, 0, "{label} prune must skip without failing");
        assert!(
            requests.is_empty(),
            "a corrupt scope map must not read the provider in {label}: {requests:?}"
        );
        let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        let row = rows.iter().find(|row| row.lease_id == "scoped-7").unwrap();
        assert_eq!(row.status, "active", "{label} must not recover the lease");
        assert_eq!(
            row.heartbeat_at, 1,
            "{label} must not rewrite the heartbeat"
        );
        assert!(directory.exists(), "{label} must not remove the worktree");
    }
}

/// A local mock that inserts a different-number legacy lease into the scanned
/// directory while it serves the first provider request, so the prune pass sees
/// the mixed ownership only at its fresh removal recheck.
struct MidPassLeaseMock {
    base: String,
    task: JoinHandle<Vec<String>>,
}

impl MidPassLeaseMock {
    fn start(db: PathBuf, identity: String, worktree: PathBuf, branch: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let responses = [REPO_JSON.to_owned(), issue_json(7, "closed", "[]")];
            let mut requests = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut inserted = false;
            for body in responses {
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(pair) => break pair,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if std::time::Instant::now() > deadline {
                                return requests;
                            }
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => return requests,
                    }
                };
                if !inserted {
                    let storage = Storage::open_at(&db).unwrap();
                    insert_lease(
                        &storage,
                        NewLease {
                            lease_id: "legacy-six",
                            identity: &identity,
                            issue: 6,
                            session: "session-legacy",
                            checkout_path: worktree.parent().unwrap().to_str().unwrap(),
                            worktree_path: worktree.to_str().unwrap(),
                            branch: &branch,
                            status: "retained",
                            created_at: 1,
                            heartbeat_at: crate::worktree::now_unix_secs(),
                        },
                    )
                    .unwrap();
                    inserted = true;
                }
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let size = stream.read(&mut buffer).unwrap();
                    if size == 0 {
                        break;
                    }
                    data.extend_from_slice(&buffer[..size]);
                    if let Some(end) = data.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8_lossy(&data).into_owned());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
            requests
        });
        Self { base, task }
    }

    fn finish(self) -> Vec<String> {
        self.task.join().unwrap()
    }
}

/// A legacy lease that arrives after the scan snapshot must keep the mixed
/// directory at the fresh recheck, before any removal, session move, or cache
/// write.
#[test]
fn production_prune_mid_pass_mixed_legacy_arrival_keeps() {
    let fixture = Fixture::new("prune-midpass-mixed");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = MidPassLeaseMock::start(
        fixture.db.path().join("leases.sqlite3"),
        fixture.identity(),
        directory.clone(),
        "phasegent/7-sync".to_owned(),
    );
    let base = mock.base.clone();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "prune must report the keep: {requests:?}");
    assert_eq!(
        requests.len(),
        2,
        "the scan must read the closing issue before the mid-pass arrival keeps at the recheck: {requests:?}"
    );
    assert!(
        directory.exists(),
        "a mid-pass legacy arrival must keep the mixed directory: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "retained",
        "the scoped lease must not be rewritten by the keep"
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "legacy-six")
            .unwrap()
            .status,
        "retained",
        "the mid-pass legacy lease must survive the keep"
    );
}

#[test]
fn scoped_sync_other_issue_not_found_keeps_current_active() {
    let fixture = Fixture::new("sync-other-notfound");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    fixture.link(8, "owner/repo");
    let directory = fixture.worktree("phasegent/7-other-notfound");
    let seven = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    let eight = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    fixture.lease(
        &storage,
        "scoped-seven",
        7,
        &directory,
        "phasegent/7-other-notfound",
        "active",
        Some(&seven),
    );
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &directory,
        "phasegent/7-other-notfound",
        "retained",
        Some(&eight),
    );
    let mock = Mock::start_status(vec![
        (200, REPO_JSON.to_owned()),
        (200, issue_json(7, "closed", "[]")),
        (404, r#"{"message":"not found"}"#.to_owned()),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(
        exit, 0,
        "an unproven other issue must fail closed: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-seven")
            .unwrap()
            .status,
        "active",
        "the current issue must stay active when another associated issue is unproven"
    );
    assert!(directory.exists());
}

#[test]
fn scoped_sync_other_issue_error_keeps_current_active() {
    let fixture = Fixture::new("sync-other-error");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    fixture.link(8, "owner/repo");
    let directory = fixture.worktree("phasegent/7-other-error");
    let seven = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    let eight = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    fixture.lease(
        &storage,
        "scoped-seven",
        7,
        &directory,
        "phasegent/7-other-error",
        "active",
        Some(&seven),
    );
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &directory,
        "phasegent/7-other-error",
        "retained",
        Some(&eight),
    );
    let mock = Mock::start_status(vec![
        (200, REPO_JSON.to_owned()),
        (200, issue_json(7, "closed", "[]")),
        (500, r#"{"message":"server error"}"#.to_owned()),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(
        exit, 0,
        "a provider error for another issue must fail: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-seven")
            .unwrap()
            .status,
        "active",
        "the current issue must stay active when another associated read errors"
    );
    assert!(directory.exists());
}

/// Serves provider responses while inserting a same-issue exact-scope lease in
/// a second, already-mixed directory, so the sync observes it only in its fresh
/// proof and must leave it active.
struct MidPassScopedMock {
    base: String,
    task: JoinHandle<Vec<String>>,
}

impl MidPassScopedMock {
    fn start(db: PathBuf, identity: String, worktree: PathBuf, branch: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let mut requests = Vec::new();
            let mut idle_deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(500);
            let mut inserted = false;
            loop {
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(pair) => break pair,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if std::time::Instant::now() > idle_deadline {
                                return requests;
                            }
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => return requests,
                    }
                };
                idle_deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
                if !inserted {
                    let storage = Storage::open_at(&db).unwrap();
                    insert_lease_scoped(
                        &storage,
                        NewLease {
                            lease_id: "late-seven",
                            identity: &identity,
                            issue: 7,
                            session: "session-late",
                            checkout_path: worktree.parent().unwrap().to_str().unwrap(),
                            worktree_path: worktree.to_str().unwrap(),
                            branch: &branch,
                            status: "active",
                            created_at: 1,
                            heartbeat_at: crate::worktree::now_unix_secs(),
                        },
                        &LeaseScope::new("forgejo", "owner/repo", 7).unwrap(),
                    )
                    .unwrap();
                    inserted = true;
                }
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let size = stream.read(&mut buffer).unwrap();
                    if size == 0 {
                        break;
                    }
                    data.extend_from_slice(&buffer[..size]);
                    if let Some(end) = data.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&data).into_owned();
                let response_body = if request.starts_with("GET /api/v1/repos/owner/repo/issues/6 ")
                {
                    issue_json(6, "open", "[]")
                } else if request.starts_with("GET /api/v1/repos/owner/repo/issues/7 ") {
                    issue_json(7, "closed", "[]")
                } else {
                    REPO_JSON.to_owned()
                };
                requests.push(request);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                    response_body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self { base, task }
    }

    fn finish(self) -> Vec<String> {
        self.task.join().unwrap()
    }
}

/// A same-issue exact-scope lease that arrives in a second mixed directory is
/// unproven, so the whole issue is kept and the late lease stays active.
#[test]
fn scoped_sync_late_same_issue_lease_in_second_mixed_directory_stays_active() {
    let fixture = Fixture::new("sync-late-second-mixed");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let first = scoped_lease(&fixture, &storage, 7, "active");
    let second = fixture.worktree("phasegent/7-late-mixed");
    fixture.lease(
        &storage,
        "legacy-six",
        6,
        &second,
        "phasegent/7-late-mixed",
        "retained",
        None,
    );
    let (_stub, _path) = opencode_stub();
    let mock = MidPassScopedMock::start(
        fixture.db.path().join("leases.sqlite3"),
        fixture.identity(),
        second.clone(),
        "phasegent/7-late-mixed".to_owned(),
    );
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(
        exit, 0,
        "the mixed late arrival keeps without failing: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    for id in ["scoped-7", "late-seven"] {
        assert_eq!(
            rows.iter().find(|row| row.lease_id == id).unwrap().status,
            "active",
            "{id} must stay active when a second mixed directory is unproven"
        );
    }
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "legacy-six")
            .unwrap()
            .status,
        "retained"
    );
    assert!(first.exists());
    assert!(second.exists());
}

/// The current active issue is natively closed, but a retained other issue on
/// the same directory is natively open: the other-open proof must keep the
/// current lease active and release nothing.
#[test]
fn scoped_sync_other_open_keeps_current_active_issue() {
    let fixture = Fixture::new("sync-other-open-current-closed");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    fixture.link(8, "owner/repo");
    let directory = fixture.worktree("phasegent/8-other-open");
    let seven = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    let eight = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    fixture.lease(
        &storage,
        "scoped-seven",
        7,
        &directory,
        "phasegent/8-other-open",
        "retained",
        Some(&seven),
    );
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &directory,
        "phasegent/8-other-open",
        "active",
        Some(&eight),
    );
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "open", "[]"),
        issue_json(8, "closed", "[]"),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "the other-open keep must not fail: {requests:?}");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-eight")
            .unwrap()
            .status,
        "active",
        "the active current issue must not be retained while the other issue is open"
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-seven")
            .unwrap()
            .status,
        "retained"
    );
    assert!(directory.exists());
}

/// An unrelated exact-scope lease elsewhere in the repository must not switch
/// a legacy issue onto the provider-aware keep path: the legacy local issue is
/// still reconciled on its own canonical directory with no Forgejo dispatcher.
#[test]
fn legacy_local_sync_with_unrelated_scoped_lease_on_another_path() {
    let fixture = Fixture::new("legacy-unrelated-scoped");
    let storage = fixture.storage();
    let local_path = fixture.db.path().join("local.sqlite3");
    let local = crate::infra::local_store::open_local_at(&local_path).unwrap();
    local
        .connection
        .execute(
            "INSERT INTO local_issues (id,title,body,status,project,tracker,author_role,created_at,updated_at,closed_at) VALUES (7,'legacy','', 'Closed','default','Task','executor',1,1,1)",
            [],
        )
        .unwrap();
    drop(local);
    let _local_env = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_path.to_string_lossy().as_ref(),
    );
    let legacy = fixture.worktree("phasegent/7-legacy");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &legacy,
        "phasegent/7-legacy",
        "active",
        None,
    );
    let foreign = fixture.worktree("phasegent/8-forgejo");
    let scope = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    fixture.lease(
        &storage,
        "scoped-eight",
        8,
        &foreign,
        "phasegent/8-forgejo",
        "retained",
        Some(&scope),
    );
    let args: Vec<String> = ["--provider", "local", "issue", "sync"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    assert_eq!(exit, 0, "the legacy local sync must reconcile");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert!(
        rows.iter()
            .find(|row| row.lease_id == "legacy-seven")
            .unwrap()
            .status
            != "active",
        "the closed legacy issue must be reconciled despite an unrelated scoped lease"
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-eight")
            .unwrap()
            .status,
        "retained",
        "the unrelated Forgejo lease must stay untouched"
    );
    assert!(!legacy.exists());
    assert!(foreign.exists());
}

/// The real sync orchestration approves the complete ownership proof, then a
/// same-number legacy row arrives before the retention transaction opens. The
/// transaction must abort all updates so the verified lease stays active.
#[test]
fn scoped_sync_post_proof_interleaving_keeps_verified_lease_active() {
    let fixture = Fixture::new("sync-post-proof-interleave");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    let identity = fixture.identity();
    let hook_directory = directory.clone();
    let hook_identity = identity.clone();
    crate::cli::issue::sync::set_retention_interleave_hook(Box::new(move |storage| {
        insert_lease(
            storage,
            NewLease {
                lease_id: "late-legacy",
                identity: &hook_identity,
                issue: 7,
                session: "session-late-legacy",
                checkout_path: hook_directory.parent().unwrap().to_str().unwrap(),
                worktree_path: hook_directory.to_str().unwrap(),
                branch: "phasegent/7-sync",
                status: "retained",
                created_at: 2,
                heartbeat_at: crate::worktree::now_unix_secs(),
            },
        )
        .unwrap();
    }));
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(7, "closed", "[]")]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(
        exit, 0,
        "a post-proof membership arrival must abort retention: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "active",
        "the verified lease must stay active when the transaction proof changed"
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "late-legacy")
            .unwrap()
            .status,
        "retained",
        "the late legacy row must survive untouched"
    );
    assert!(directory.exists());
}

#[test]
fn production_prune_explicit_redmine_never_uses_forgejo_default() {
    let fixture = Fixture::new("prune-redmine-fg-default");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = Mock::start(vec![REPO_JSON.to_owned()]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "redmine",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0);
    assert!(
        requests.is_empty(),
        "explicit Redmine must not contact the Forgejo default: {requests:?}"
    );
    assert!(
        directory.exists(),
        "explicit Redmine must keep a Forgejo scoped worktree"
    );
}

#[test]
fn production_prune_omitted_provider_with_forgejo_default_fails_closed() {
    let fixture = Fixture::new("prune-omitted-fg-default");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = ["worktree", "prune", "--repo", &repo, "--format", "json"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(
        exit, 0,
        "an omitted selection resolving to Forgejo must fail closed"
    );
    assert!(
        requests.is_empty(),
        "an omitted selection must not operate a Forgejo default: {requests:?}"
    );
    assert!(
        directory.exists(),
        "the unresolved selection must not remove"
    );
}

#[test]
fn production_prune_scoped_recovery_foreign_context_leaves_leases() {
    let fixture = Fixture::new("prune-recovery-foreign");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    storage
        .connection
        .execute(
            "UPDATE worktree_leases SET heartbeat_at = 1 WHERE lease_id = 'scoped-7'",
            [],
        )
        .unwrap();
    let (_stub, _path) = opencode_stub();
    let mock = ZeroMock::start();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "local",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--format",
        "json",
        "--release-stale",
        "--stale-days",
        "0",
        "--reason",
        "fixture",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0);
    assert!(
        requests.is_empty(),
        "a legacy recovery context must not read Forgejo: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let row = rows.iter().find(|row| row.lease_id == "scoped-7").unwrap();
    assert_eq!(row.status, "active", "foreign recovery must not release");
    assert_eq!(row.heartbeat_at, 1, "foreign recovery must not rewrite");
    assert!(directory.exists());
}

#[test]
fn scoped_sync_same_number_legacy_on_another_path_makes_zero_requests() {
    let fixture = Fixture::new("sync-same-number-legacy-cli");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    let legacy = fixture.worktree("phasegent/7-legacy-other");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &legacy,
        "phasegent/7-legacy-other",
        "retained",
        None,
    );
    let mock = ZeroMock::start();
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0);
    assert!(
        requests.is_empty(),
        "same-number legacy history must keep before any HTTP: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "active"
    );
    assert!(directory.exists());
    assert!(legacy.exists());
}

#[test]
fn forgejo_create_binding_rejects_a_later_omitted_provider() {
    let fixture = Fixture::new("create-binding-omitted");
    fixture.storage();
    use crate::git_runner::GitRunner;
    crate::git_runner::ProcessGitRunner::in_directory(fixture.repo.path())
        .run(&[
            "remote",
            "add",
            "origin",
            "https://example.test/owner/repo.git",
        ])
        .unwrap();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        LABELS_JSON.to_owned(),
        "[]".to_owned(),
        issue_json(7, "open", r#"[{"id":1,"name":"status/New"}]"#),
    ]);
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--output",
        "json",
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "create",
        "--title",
        "fixture",
        "--body",
        "fixture",
        "--session",
        "session-create-binding",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = in_dir(fixture.repo.path(), || {
        crate::cli::run_with_role(args, Some("orchestrator"))
    });
    assert_eq!(exit, 0, "issue create must succeed");
    let requests = mock.finish();
    assert!(requests.iter().any(|request| request.starts_with("POST ")));
    let runner = crate::git_runner::ProcessGitRunner::in_directory(fixture.repo.path());
    let links = crate::branch_links::compat::read_branch_link_rows(&runner, "feat/7").unwrap();
    assert!(
        links.iter().any(|row| row.status == "linked"
            && row.issue.provider == "forgejo"
            && row.issue.project == "owner/repo"
            && row.issue.external_id == "7"),
        "created issue must have a durable binding on its branch: {links:?}"
    );
    let switched = runner.run(&["switch", "feat/7"]).unwrap();
    assert_eq!(switched.status, 0);

    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "local");
    let get_mock = ZeroMock::start();
    let get_args: Vec<String> = ["issue", "get", "7"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let get_exit = run_as(fixture.repo.path(), "orchestrator", &get_args);
    let get_requests = get_mock.finish();
    assert_ne!(
        get_exit, 0,
        "the durable created binding must reject an omitted provider"
    );
    assert!(
        get_requests.is_empty(),
        "the rejected read must not reach any provider: {get_requests:?}"
    );
}

#[test]
fn scoped_sync_rejects_incompatible_forgejo_project_override_before_http() {
    let fixture = Fixture::new("sync-fg-project-override");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "active");
    let mock = ZeroMock::start();
    let base = mock.base.clone();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "--project-id",
        "23",
        "issue",
        "sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "Forgejo must reject --project-id");
    assert!(
        requests.is_empty(),
        "rejection must precede HTTP: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-7")
            .unwrap()
            .status,
        "active"
    );
    assert!(directory.exists());
}

#[test]
fn acquire_rejects_incompatible_forgejo_close_status_before_lease_write() {
    let fixture = Fixture::new("acquire-incompatible-close");
    let storage = fixture.storage();
    let mock = ZeroMock::start();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "--close-status-id",
        "7",
        "worktree",
        "acquire",
        "--issue",
        "7",
        "--session",
        "fixture-session",
        "--no-sync",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert!(requests.is_empty());
    assert!(rows.is_empty(), "no lease may be written: {rows:?}");
    assert_ne!(exit, 0, "incompatible Forgejo flags must fail");
}

#[test]
fn acquire_rejects_incompatible_flags_before_preexisting_taxi_candidates() {
    let fixture = Fixture::new("acquire-incompatible-preexisting");
    let storage = fixture.storage();
    fixture.link(8, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 8, "retained");
    let before = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let mock = ZeroMock::start();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "--project-id",
        "23",
        "worktree",
        "acquire",
        "--issue",
        "7",
        "--session",
        "fixture-session",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let after = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert!(
        requests.is_empty(),
        "taxi/acquire must not reach HTTP: {requests:?}"
    );
    assert_ne!(exit, 0, "incompatible Forgejo flags must fail");
    assert_eq!(after.len(), before.len(), "no lease may be added");
    for (before_row, after_row) in before.iter().zip(after.iter()) {
        assert_eq!(before_row.lease_id, after_row.lease_id);
        assert_eq!(before_row.status, after_row.status);
        assert_eq!(before_row.heartbeat_at, after_row.heartbeat_at);
    }
    assert!(directory.exists());
}

/// Two fixture providers with different answers: the explicit Local source
/// says issue 9 is open while the default Forgejo fixture says closed. Only
/// the explicit target may authorize cleanup, and the default is never read.
#[test]
fn legacy_close_closure_uses_only_the_explicit_provider_endpoint() {
    let fixture = Fixture::new("legacy-close-two-providers");
    let storage = fixture.storage();
    let local_path = fixture.db.path().join("explicit-local.sqlite3");
    let local = crate::infra::local_store::open_local_at(&local_path).unwrap();
    local
        .connection
        .execute(
            "INSERT INTO local_issues (id,title,body,status,project,tracker,author_role,created_at,updated_at,closed_at) VALUES (9,'other','', 'New','default','Task','executor',1,1,NULL)",
            [],
        )
        .unwrap();
    drop(local);
    let _local_env = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_path.to_string_lossy().as_ref(),
    );
    let default_mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &default_mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let context = crate::cli::provider_context::ProviderContext::from_parts(
        Some(crate::providers::ProviderKind::Local),
        None,
        None,
        None,
        None,
    );
    let closure = crate::cli::issue_closure::CurrentIssueClosure::new_context(
        fixture.repo.path(),
        Role::Orchestrator,
        context,
    );
    use crate::lifecycle::cleanup::closure::DirectoryIssueClosure;
    assert!(
        closure.confirm_others_closed(7, &[9]).is_err(),
        "the explicit Local source reports issue 9 open and must keep"
    );
    let requests = default_mock.finish();
    assert!(
        requests.is_empty(),
        "the default Forgejo source must never be read: {requests:?}"
    );
    let local = crate::infra::local_store::open_local_at(&local_path).unwrap();
    local
        .connection
        .execute(
            "UPDATE local_issues SET status = 'Closed', closed_at = 2 WHERE id = 9",
            [],
        )
        .unwrap();
    drop(local);
    let closed_context = crate::cli::provider_context::ProviderContext::from_parts(
        Some(crate::providers::ProviderKind::Local),
        None,
        None,
        None,
        None,
    );
    let closed_closure = crate::cli::issue_closure::CurrentIssueClosure::new_context(
        fixture.repo.path(),
        Role::Orchestrator,
        closed_context,
    );
    assert!(
        closed_closure.confirm_others_closed(7, &[9]).is_ok(),
        "the explicit Local source reports issue 9 closed and authorizes cleanup"
    );
}

#[test]
fn production_legacy_close_uses_only_the_explicit_provider_answers() {
    for (label, other_status, expect_removed) in
        [("open", "New", false), ("closed", "Closed", true)]
    {
        let fixture = Fixture::new(&format!("legacy-close-{label}"));
        let storage = fixture.storage();
        let local_path = fixture.db.path().join("explicit-local.sqlite3");
        let local = crate::infra::local_store::open_local_at(&local_path).unwrap();
        local
            .connection
            .execute(
                "INSERT INTO local_issues (id,title,body,status,project,tracker,author_role,created_at,updated_at,closed_at) VALUES (7,'current','', 'Resolved','default','Task','executor',1,1,NULL)",
                [],
            )
            .unwrap();
        local
            .connection
            .execute(
                "INSERT INTO local_issues (id,title,body,status,project,tracker,author_role,created_at,updated_at,closed_at) VALUES (9,'other','', ?1,'default','Task','executor',1,1,?2)",
                rusqlite::params![other_status, if other_status == "Closed" { Some(2i64) } else { None }],
            )
            .unwrap();
        drop(local);
        let _local_env = EnvGuard::set(
            "PHASEGENT_LOCAL_DB_PATH",
            local_path.to_string_lossy().as_ref(),
        );
        let directory = fixture.worktree("phasegent/7-legacy-close");
        fixture.lease(
            &storage,
            "legacy-seven",
            7,
            &directory,
            "phasegent/7-legacy-close",
            "active",
            None,
        );
        fixture.lease(
            &storage,
            "legacy-nine",
            9,
            &directory,
            "phasegent/7-legacy-close",
            "retained",
            None,
        );
        let (_stub, _path) = opencode_stub();
        let default_mock = Mock::start(vec![
            REPO_JSON.to_owned(),
            issue_json(9, "closed", "[]"),
            issue_json(9, "closed", "[]"),
        ]);
        storage
            .save_forgejo_auth(
                Role::Orchestrator,
                &default_mock.base,
                "owner/repo",
                "fixture-token",
            )
            .unwrap();
        let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
        let args: Vec<String> = [
            "--provider",
            "local",
            "issue",
            "close",
            "7",
            "--worktree-session",
            "fixture-session",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        let requests = default_mock.finish();
        assert_eq!(exit, 0, "{label} close must succeed");
        assert!(
            requests.is_empty(),
            "the configured default must never be read during {label} close: {requests:?}"
        );
        assert_eq!(
            directory.exists(),
            !expect_removed,
            "the explicit {label} answer must control directory cleanup"
        );
    }
}

#[test]
fn rejected_stored_default_does_not_migrate_during_taxi_list() {
    let fixture = Fixture::new("stored-default-list-no-migration");
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let path = fixture.db.path().join("leases.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); \
             INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER', 'forgejo');",
        )
        .unwrap();
    let count_tables = || -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let before = count_tables();
    let args: Vec<String> = ["worktree", "list"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let after = count_tables();
    assert_ne!(exit, 0, "implicit Forgejo default must be rejected");
    assert_eq!(
        after, before,
        "the rejected taxi/list route must not initialize or migrate the store"
    );
}

struct RouteCounter {
    base: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    task: Option<JoinHandle<usize>>,
}

impl RouteCounter {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = stop.clone();
        let task = thread::spawn(move || {
            let mut count = 0;
            loop {
                match listener.accept() {
                    Ok((_stream, _)) => count += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if done.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(error) => panic!("counting listener: {error}"),
                }
            }
            count
        });
        Self {
            base,
            stop,
            task: Some(task),
        }
    }

    fn finish(mut self) -> usize {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.task.take().unwrap().join().unwrap()
    }
}

impl Drop for RouteCounter {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

#[test]
fn route_counter_counts_a_connection_before_join() {
    let counter = RouteCounter::start();
    let address = counter.base.strip_prefix("http://").unwrap();
    drop(std::net::TcpStream::connect(address).unwrap());
    assert_eq!(counter.finish(), 1);
}

fn complete_fixture_snapshot(connection: &rusqlite::Connection) -> Vec<(String, Vec<String>)> {
    let mut schema = connection
        .prepare("SELECT type, name, COALESCE(sql,'') FROM sqlite_master ORDER BY type, name")
        .unwrap();
    let objects: Vec<(String, String, String)> = schema
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut snapshot = vec![("schema".to_owned(), vec![format!("{objects:?}")])];
    for (_, name, _) in objects.iter().filter(|(kind, _, _)| kind == "table") {
        let quoted = name.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{quoted}\""))
            .unwrap();
        let columns = statement.column_count();
        let mut rows: Vec<String> = statement
            .query_map([], |row| {
                let mut values = Vec::new();
                for index in 0..columns {
                    values.push(format!("{:?}", row.get_ref(index)?));
                }
                Ok(format!("{values:?}"))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.sort();
        snapshot.push((name.clone(), rows));
    }
    snapshot
}

fn fixture_permissions(path: &Path) -> (bool, Option<u32>) {
    let permissions = std::fs::metadata(path).unwrap().permissions();
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(permissions.mode())
    };
    #[cfg(not(unix))]
    let mode = None;
    (permissions.readonly(), mode)
}

#[test]
fn admission_read_only_wal_does_not_create_sidecars() {
    let fixture = Fixture::new("read-only-wal-sidecars-review");

    // Case 1: WAL-mode database whose native sidecars are absent once the
    // writer has closed. SQLite owns their lifecycle; the fixture never
    // manually deletes a sidecar.
    let absent_path = fixture.db.path().join("absent-sidecars.sqlite3");
    let writer = rusqlite::Connection::open(&absent_path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER', 'forgejo');")
        .unwrap();
    let before_snapshot = logical_snapshot(&absent_path);
    drop(writer);
    let wal = fixture.db.path().join("absent-sidecars.sqlite3-wal");
    let shm = fixture.db.path().join("absent-sidecars.sqlite3-shm");
    assert!(!wal.exists() && !shm.exists());
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", absent_path.to_str().unwrap());
    #[cfg(unix)]
    let mode_before = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&absent_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    let store = Storage::open_existing_read_only().unwrap().unwrap();
    assert!(store.table_exists("global_setting").unwrap());
    assert_eq!(
        store
            .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .unwrap(),
        Some("forgejo".to_owned())
    );
    assert!(
        store
            .connection
            .execute("INSERT INTO global_setting VALUES ('x','y')", [])
            .is_err(),
        "read-only admission must reject SQL writes"
    );
    assert_eq!(logical_snapshot(&absent_path), before_snapshot);
    drop(store);
    assert_eq!(logical_snapshot(&absent_path), before_snapshot);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&absent_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            mode_before,
            "admission must not change file permissions"
        );
    }
    for entry in std::fs::read_dir(fixture.db.path()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            matches!(
                name.as_str(),
                "absent-sidecars.sqlite3"
                    | "absent-sidecars.sqlite3-wal"
                    | "absent-sidecars.sqlite3-shm"
            ),
            "admission created an unrelated file: {name}"
        );
    }
    let _ = &wal;
    let _ = &shm;

    // Case 2: an active committed WAL, including a provider-default change
    // committed only in that WAL, remains visible through the read-only probe.
    let committed_path = fixture.db.path().join("committed-wal.sqlite3");
    let writer = rusqlite::Connection::open(&committed_path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER', 'forgejo'); UPDATE global_setting SET value='redmine' WHERE name='PHASEGENT_DEFAULT_PROVIDER';")
        .unwrap();
    assert!(fixture.db.path().join("committed-wal.sqlite3-wal").exists());
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", committed_path.to_str().unwrap());
    let before_snapshot = logical_snapshot(&committed_path);
    let store = Storage::open_existing_read_only().unwrap().unwrap();
    assert_eq!(
        store
            .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .unwrap(),
        Some("redmine".to_owned()),
        "committed WAL provider-default change must be visible"
    );
    assert_eq!(logical_snapshot(&committed_path), before_snapshot);
    drop(store);
    drop(writer);
}

fn logical_snapshot(path: &Path) -> Vec<(String, String)> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut statement = connection
        .prepare("SELECT name, COALESCE(sql,'') FROM sqlite_master ORDER BY name")
        .unwrap();
    let mut snapshot: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    let data: String = connection
        .query_row(
            "SELECT COALESCE(group_concat(name || '=' || value, '|'), '') FROM global_setting",
            [],
            |row| row.get(0),
        )
        .unwrap();
    snapshot.push(("data".to_owned(), data));
    snapshot
}

#[test]
fn admission_matrix_preserves_complete_state_and_counts_each_route() {
    let states = [
        "missing",
        "old",
        "initialized",
        #[cfg(unix)]
        "inaccessible",
    ];
    for state in states {
        let fixture = Fixture::new(&format!("complete-admission-{state}"));
        let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        let _config = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            fixture.db.path().join("missing.toml").to_str().unwrap(),
        );
        let _provider =
            (state == "missing").then(|| EnvGuard::set("PHASEGENT_PROVIDER", "forgejo"));
        let parent = if state == "inaccessible" {
            let dir = fixture.db.path().join("blocked");
            std::fs::create_dir(&dir).unwrap();
            dir
        } else {
            fixture.db.path().to_path_buf()
        };
        let path = parent.join("leases.sqlite3");
        let _db = EnvGuard::set("PHASEGENT_DB_PATH", path.to_str().unwrap());
        let connection = if state == "missing" {
            None
        } else if state == "initialized" {
            let storage = fixture.storage();
            storage
                .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
                .unwrap();
            fixture.link(8, "owner/repo");
            let directory = scoped_lease(&fixture, &storage, 8, "retained");
            assert!(directory.exists());
            Some(storage.connection)
        } else {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection.execute_batch("CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER','forgejo');").unwrap();
            Some(connection)
        };
        let initial = connection.as_ref().map(complete_fixture_snapshot);
        let parent_mode = fixture_permissions(&parent);
        let file_mode = path.exists().then(|| fixture_permissions(&path));
        let repo = fixture.repo.path().to_string_lossy().to_string();
        let routes: &[&[&str]] = &[
            &[
                "worktree",
                "acquire",
                "--issue",
                "7",
                "--session",
                "fixture-session",
                "--no-sync",
            ],
            &[
                "worktree",
                "acquire",
                "--issue",
                "7",
                "--session",
                "fixture-session",
            ],
            &["worktree", "list"],
            &[
                "worktree",
                "prune",
                "--repo",
                &repo,
                "--release-stale",
                "--stale-days",
                "0",
                "--reason",
                "fixture",
            ],
        ];
        for route in routes {
            let counter = RouteCounter::start();
            let mut args = vec!["--api-base".to_owned(), counter.base.clone()];
            args.extend(route.iter().map(|part| (*part).to_owned()));
            #[cfg(unix)]
            if state == "inaccessible" {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
                assert_eq!(
                    std::fs::metadata(&path).unwrap_err().kind(),
                    std::io::ErrorKind::PermissionDenied
                );
            }
            let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
            let _blocked_mode = fixture_permissions(&parent);
            let requests = counter.finish();
            #[cfg(unix)]
            if state == "inaccessible" {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &parent,
                    std::fs::Permissions::from_mode(parent_mode.1.unwrap()),
                )
                .unwrap();
                assert_eq!(_blocked_mode.1.unwrap() & 0o777, 0, "{route:?}");
            }
            assert_ne!(exit, 0, "{state}: {route:?}");
            let expected = if state == "inaccessible" {
                "could not inspect phasegent database"
            } else {
                "explicit --provider forgejo"
            };
            assert!(stderr.contains(expected), "{state}: {route:?}: {stderr}");
            assert_eq!(requests, 0, "{state}: {route:?}");
            assert_eq!(
                fixture_permissions(&parent),
                parent_mode,
                "{state}: {route:?}"
            );
            if let Some(connection) = connection.as_ref() {
                assert_eq!(
                    Some(complete_fixture_snapshot(connection)),
                    initial,
                    "{state}: {route:?}"
                );
                assert_eq!(
                    Some(fixture_permissions(&path)),
                    file_mode,
                    "{state}: {route:?}"
                );
            } else {
                assert!(!path.exists(), "{state}: {route:?}");
                assert_eq!(
                    std::fs::read_dir(&parent).unwrap().count(),
                    0,
                    "{state}: {route:?}"
                );
            }
        }
    }
}

#[test]
fn old_stored_default_is_rejected_across_every_entry_route() {
    let fixture = Fixture::new("old-store-route-matrix");
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let path = fixture.db.path().join("leases.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); \
             INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER', 'forgejo');",
        )
        .unwrap();
    let count_tables = || -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let before = count_tables();
    let schema_before: String = connection
        .query_row(
            "SELECT group_concat(name || ':' || COALESCE(sql,''), '|') FROM (SELECT name, sql FROM sqlite_master ORDER BY name)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let diagnostic = crate::cli::provider_context::ProviderContext::default()
        .validate(Role::Orchestrator)
        .unwrap_err()
        .json()
        .get("message")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    assert!(
        diagnostic.contains("explicit --provider forgejo"),
        "old store must report the explicit-selection hint: {diagnostic}"
    );
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let commands: Vec<Vec<String>> = vec![
        vec![
            "worktree",
            "acquire",
            "--issue",
            "7",
            "--session",
            "fixture-session",
            "--no-sync",
        ],
        vec![
            "worktree",
            "acquire",
            "--issue",
            "7",
            "--session",
            "fixture-session",
        ],
        vec!["worktree", "list"],
        vec![
            "worktree",
            "prune",
            "--repo",
            &repo,
            "--release-stale",
            "--stale-days",
            "0",
            "--reason",
            "fixture",
        ],
    ]
    .into_iter()
    .map(|command| command.into_iter().map(str::to_owned).collect())
    .collect();
    for command in commands {
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &command);
        assert_ne!(
            exit, 0,
            "route {command:?} must reject the old stored default"
        );
        assert!(
            stderr.contains("explicit --provider forgejo"),
            "route {command:?} must report its own explicit-selection diagnostic: {stderr}"
        );
        assert_eq!(
            count_tables(),
            before,
            "route {command:?} must not initialize or migrate the old store"
        );
        let schema_after: String = connection
            .query_row(
                "SELECT group_concat(name || ':' || COALESCE(sql,''), '|') FROM (SELECT name, sql FROM sqlite_master ORDER BY name)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            schema_after, schema_before,
            "route {command:?} changed the old schema"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT value FROM global_setting WHERE name='PHASEGENT_DEFAULT_PROVIDER'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "forgejo",
            "route {command:?} changed stored default data"
        );
    }
}

#[test]
fn initialized_stored_forgejo_default_is_rejected_without_lease_mutation() {
    let fixture = Fixture::new("initialized-store-route-matrix");
    let storage = fixture.storage();
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let mock = ZeroMock::start();
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
        .unwrap();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let diagnostic = crate::cli::provider_context::ProviderContext::default()
        .validate(Role::Orchestrator)
        .unwrap_err()
        .json()
        .get("message")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    assert!(
        diagnostic.contains("explicit --provider forgejo"),
        "initialized store must report the explicit-selection hint: {diagnostic}"
    );
    fixture.link(8, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 8, "retained");
    let before = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let commands: Vec<Vec<String>> = vec![
        vec![
            "worktree",
            "acquire",
            "--issue",
            "7",
            "--session",
            "fixture-session",
            "--no-sync",
        ],
        vec![
            "worktree",
            "acquire",
            "--issue",
            "7",
            "--session",
            "fixture-session",
        ],
        vec!["worktree", "list"],
        vec![
            "worktree",
            "prune",
            "--repo",
            &repo,
            "--release-stale",
            "--stale-days",
            "0",
            "--reason",
            "fixture",
        ],
    ]
    .into_iter()
    .map(|command| command.into_iter().map(str::to_owned).collect())
    .collect();
    for command in commands {
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &command);
        assert_ne!(
            exit, 0,
            "route {command:?} must reject the stored Forgejo default"
        );
        assert!(
            stderr.contains("explicit --provider forgejo"),
            "route {command:?} must report its own explicit-selection diagnostic: {stderr}"
        );
        let after = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        assert_eq!(after.len(), before.len(), "route {command:?} added a lease");
        for (before_row, after_row) in before.iter().zip(after.iter()) {
            assert_eq!(before_row.lease_id, after_row.lease_id);
            assert_eq!(before_row.status, after_row.status);
            assert_eq!(before_row.heartbeat_at, after_row.heartbeat_at);
        }
        assert!(directory.exists());
    }
    let requests = mock.finish();
    assert!(
        requests.is_empty(),
        "no route may contact a provider: {requests:?}"
    );
}

#[cfg(unix)]
#[test]
fn inaccessible_stored_default_is_rejected_across_every_entry_route() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("inaccessible-store-route-matrix");
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let blocked = fixture.db.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    let path = blocked.join("leases.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); \
             INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER', 'forgejo');",
        )
        .unwrap();
    let count_tables = || -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let before = count_tables();
    let main_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", path.to_str().unwrap());
    let original = std::fs::metadata(&blocked).unwrap().permissions();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let metadata_error = std::fs::metadata(&path).err();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let commands: Vec<Vec<String>> = vec![
        vec![
            "worktree",
            "acquire",
            "--issue",
            "7",
            "--session",
            "fixture-session",
            "--no-sync",
        ],
        vec![
            "worktree",
            "acquire",
            "--issue",
            "7",
            "--session",
            "fixture-session",
        ],
        vec!["worktree", "list"],
        vec![
            "worktree",
            "prune",
            "--repo",
            &repo,
            "--release-stale",
            "--stale-days",
            "0",
            "--reason",
            "fixture",
        ],
    ]
    .into_iter()
    .map(|command| command.into_iter().map(str::to_owned).collect())
    .collect();
    let mut exits = Vec::new();
    let mut stderrs = Vec::new();
    for command in &commands {
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), command);
        exits.push(exit);
        stderrs.push(stderr);
    }
    let blocked_diagnostic = crate::cli::provider_context::ProviderContext::default()
        .validate(Role::Orchestrator)
        .unwrap_err()
        .json()
        .get("message")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    std::fs::set_permissions(&blocked, original).unwrap();
    if metadata_error.is_none() {
        return;
    }
    assert_eq!(
        metadata_error.unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(
        exits.iter().all(|exit| *exit != 0),
        "every route must reject unreadable evidence: {exits:?}"
    );
    assert!(
        stderrs
            .iter()
            .all(|stderr| stderr.contains("could not inspect phasegent database")),
        "every route must report its own metadata diagnostic: {stderrs:?}"
    );
    assert_eq!(
        count_tables(),
        before,
        "unreadable store must stay unchanged"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        main_mode,
        "admission must preserve the main database permissions"
    );
    assert!(
        blocked_diagnostic.contains("could not inspect phasegent database"),
        "unreadable evidence must report a metadata error: {blocked_diagnostic}"
    );
}

#[test]
fn prune_preview_keeps_legacy_same_number_and_mixed_scope_without_writes() {
    let fixture = Fixture::new("prune-preview-scope-matrix");
    let storage = fixture.storage();
    let legacy_only = fixture.worktree("phasegent/9-legacy-only");
    fixture.lease(
        &storage,
        "legacy-nine-preview",
        9,
        &legacy_only,
        "phasegent/9-legacy-only",
        "retained",
        None,
    );
    let scoped_seven = scoped_lease(&fixture, &storage, 7, "retained");
    let same_number_other = fixture.worktree("phasegent/7-legacy-preview");
    fixture.lease(
        &storage,
        "legacy-seven",
        7,
        &same_number_other,
        "phasegent/7-legacy-preview",
        "retained",
        None,
    );
    let mixed = fixture.worktree("phasegent/8-mixed-preview");
    fixture.lease(
        &storage,
        "legacy-six",
        6,
        &mixed,
        "phasegent/8-mixed-preview",
        "retained",
        None,
    );
    let eight = LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    fixture.lease(
        &storage,
        "scoped-eight-preview",
        8,
        &mixed,
        "phasegent/8-mixed-preview",
        "retained",
        Some(&eight),
    );
    let (_stub, _path) = opencode_stub();
    let mock = ZeroMock::start();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--dryrun",
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "preview must report the keeps");
    assert!(
        requests.is_empty(),
        "preview keeps must precede HTTP: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    for row in &rows {
        assert_eq!(
            row.status, "retained",
            "preview changed {}: {:?}",
            row.lease_id, row
        );
    }
    for path in [&legacy_only, &scoped_seven, &same_number_other, &mixed] {
        assert!(path.exists(), "preview removed {}", path.display());
    }
}

#[test]
fn prune_preview_scans_matching_scoped_candidate_without_writes() {
    let fixture = Fixture::new("prune-preview-positive-match");
    let storage = fixture.storage();
    fixture.link(10, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 10, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(10, "closed", "[]"),
        issue_json(10, "closed", "[]"),
    ]);
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--dryrun",
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, stdout, _stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "matching preview must succeed: {requests:?}");
    assert!(
        !requests.is_empty(),
        "the matching candidate must be scanned"
    );
    assert!(
        stdout.contains("\"prunable\":true") && stdout.contains("\"result\":\"prunable\""),
        "matching preview must report the actual prunable candidate: {stdout}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped-10")
            .unwrap()
            .status,
        "retained"
    );
    assert!(directory.exists());
}

#[test]
fn prune_preview_foreign_scoped_absent_path_keeps_without_requests_or_writes() {
    let fixture = Fixture::new("prune-preview-foreign-absent");
    let storage = fixture.storage();
    let missing = fixture.repo.path().join("missing-foreign-scoped");
    let scope = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    fixture.lease(
        &storage,
        "scoped-seven-absent",
        7,
        &missing,
        "phasegent/7-absent-foreign",
        "retained",
        Some(&scope),
    );
    let (_stub, _path) = opencode_stub();
    let mock = ZeroMock::start();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "other/repo",
        "worktree",
        "prune",
        "--repo",
        &repo,
        "--dryrun",
        "--format",
        "json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    assert_eq!(exit, 0);
    let clean_args: Vec<String> = args
        .iter()
        .filter(|arg| *arg != "--dryrun")
        .cloned()
        .collect();
    let clean_exit = run_as(fixture.repo.path(), "orchestrator", &clean_args);
    let requests = mock.finish();
    assert_eq!(clean_exit, 0);
    assert!(
        requests.is_empty(),
        "foreign preview and clean must not read: {requests:?}"
    );
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "retained");
    assert!(!missing.exists());
}

#[test]
fn absent_release_rejects_foreign_scope_change_and_active_owner_after_proof() {
    for (label, mutate) in [("foreign-sidecar", 0u8), ("active-owner", 1u8)] {
        let fixture = Fixture::new(&format!("absent-proof-change-{label}"));
        let storage = fixture.storage();
        let path = fixture.repo.path().join(format!("missing-{label}"));
        let scope = LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
        fixture.lease(
            &storage,
            "scoped-seven",
            7,
            &path,
            "phasegent/7-absent",
            "retained",
            Some(&scope),
        );
        let targets = vec![(
            "scoped-seven".to_owned(),
            "retained".to_owned(),
            path.to_str().unwrap().to_owned(),
        )];
        let canonical = crate::worktree::CanonicalPaths::default().resolve(&path);
        let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        let scopes =
            crate::worktree::lease_scope::scope_map_for_repo(&storage, &fixture.identity())
                .unwrap();
        let approved =
            crate::worktree::leases::directory_ownership_proof(&canonical, &rows, &scopes);
        if mutate == 0 {
            storage
                .connection
                .execute(
                    "UPDATE worktree_lease_scopes SET provider = 'redmine' WHERE lease_id = 'scoped-seven'",
                    [],
                )
                .unwrap();
        } else {
            fixture.lease(
                &storage,
                "late-active-eight",
                8,
                &path,
                "phasegent/8-active",
                "active",
                None,
            );
        }
        let after_injection = complete_fixture_snapshot(&storage.connection);
        let result = crate::worktree::leases::release_absent_leases_proven(
            &storage,
            &fixture.identity(),
            &targets,
            &approved,
        );
        assert!(
            result.is_err(),
            "{label} must abort absent release: {result:?}"
        );
        assert_eq!(
            complete_fixture_snapshot(&storage.connection),
            after_injection,
            "{label} must preserve all post-injection tables and rows"
        );
        let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        assert_eq!(
            rows.iter()
                .find(|row| row.lease_id == "scoped-seven")
                .unwrap()
                .status,
            "retained",
            "{label} must preserve the scoped lease"
        );
        if mutate == 0 {
            let provider: String = storage
                .connection
                .query_row(
                    "SELECT provider FROM worktree_lease_scopes WHERE lease_id='scoped-seven'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(provider, "redmine", "foreign sidecar must remain unchanged");
        } else {
            assert_eq!(
                rows.iter()
                    .find(|row| row.lease_id == "late-active-eight")
                    .unwrap()
                    .status,
                "active",
                "arriving active owner must remain active"
            );
        }
    }
}
