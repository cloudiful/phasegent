//! Deterministic full-CLI Forgejo tests (issue 1201 P3).
//!
//! These drive the real `issue create` / `issue close` executors against a
//! local mock HTTP server and an isolated SQLite lease store, so the
//! scoped-acquisition and exact-scope-release wiring is proven end to end
//! rather than only through the manual provider/CLI seams.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::thread::{self, JoinHandle};

use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::worktree::registry::{self, NewAssociation, NewDirectory, stable_directory_id};
use crate::worktree::{
    DirectoryRecord, ProcessWorktreeRunner, WorktreeError, WorktreeScope, repo_identity,
};

fn list_all_for_repo(
    storage: &Storage,
    identity: &str,
) -> Result<Vec<DirectoryRecord>, WorktreeError> {
    registry::load_records(storage, Some(identity))
}

#[path = "../tests/support/broker.rs"]
mod identity_broker;

const REPO_JSON: &str = r#"{"id":17,"name":"repo","full_name":"owner/repo"}"#;

/// A repository response whose `full_name` matches the per-operation scope, so
/// a multi-repository test never trips the response/scope consistency check.
fn repo_json(full_name: &str) -> String {
    let name = full_name.rsplit('/').next().unwrap_or(full_name);
    format!(r#"{{"id":17,"name":"{name}","full_name":"{full_name}"}}"#)
}

/// The placeholder trusted instance seeded by [`Fixture::attested_storage`].
/// Each fixture mock replaces it with its own loopback base, so the test still
/// exercises the real stored-instance trust check and never a bypass.
const FIXTURE_UNATTESTED_INSTANCE: &str = "https://fixture.invalid";

fn issue_json(number: u64, state: &str, labels: &str) -> String {
    format!(
        r#"{{"id":{},"number":{number},"title":"fixture","body":"fixture","state":"{state}","html_url":"https://example.test/owner/repo/issues/{number}","labels":{labels}}}"#,
        500 + number
    )
}

struct Mock {
    base: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    task: Option<JoinHandle<Vec<String>>>,
}

impl Mock {
    fn start(responses: Vec<String>) -> Self {
        Self::start_status(responses.into_iter().map(|body| (200, body)).collect())
    }

    fn start_status(responses: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let serve_base = base.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = stop.clone();
        let task = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let body = body.replace("__BASE__", &serve_base);
                let mut served = false;
                while !served {
                    if done.load(std::sync::atomic::Ordering::SeqCst) {
                        drain_requests(
                            &listener,
                            &mut requests,
                            std::time::Instant::now() + MOCK_DRAIN_TOTAL,
                        );
                        return requests;
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => match read_request(
                            &mut stream,
                            Some(&done),
                            std::time::Instant::now() + MOCK_REQUEST_TOTAL,
                        ) {
                            Ok(data) => {
                                requests.push(data);
                                let response = format!(
                                    "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    if status == 200 { "OK" } else { "Error" },
                                    body.len()
                                );
                                let _ = stream.write_all(response.as_bytes());
                                served = true;
                            }
                            Err(error) => {
                                requests.push(format!("<{error}>"));
                                return requests;
                            }
                        },
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if done.load(std::sync::atomic::Ordering::SeqCst) {
                                drain_requests(
                                    &listener,
                                    &mut requests,
                                    std::time::Instant::now() + MOCK_DRAIN_TOTAL,
                                );
                                return requests;
                            }
                            thread::sleep(std::time::Duration::from_millis(2));
                        }
                        Err(_) => return requests,
                    }
                }
            }
            // Every scripted response was served; keep the endpoint alive until
            // the test completes so stray traffic is observed, never waited on.
            loop {
                if done.load(std::sync::atomic::Ordering::SeqCst) {
                    drain_requests(
                        &listener,
                        &mut requests,
                        std::time::Instant::now() + MOCK_DRAIN_TOTAL,
                    );
                    return requests;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => match read_request(
                        &mut stream,
                        Some(&done),
                        std::time::Instant::now() + MOCK_REQUEST_TOTAL,
                    ) {
                        Ok(data) => requests.push(data),
                        Err(error) => requests.push(format!("<{error}>")),
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if done.load(std::sync::atomic::Ordering::SeqCst) {
                            drain_requests(
                                &listener,
                                &mut requests,
                                std::time::Instant::now() + MOCK_DRAIN_TOTAL,
                            );
                            return requests;
                        }
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => return requests,
                }
            }
        });
        Self {
            base,
            stop,
            task: Some(task),
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.task
            .take()
            .unwrap()
            .join()
            .expect("mock response server panicked")
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

/// A slow peer can trickle below the idle timeout for as long as it likes, so
/// each request also has a total deadline and a stop-aware loop. Shutdown
/// drains share one deadline so queued sockets cannot extend teardown.
const MOCK_REQUEST_IDLE: std::time::Duration = std::time::Duration::from_millis(100);
const MOCK_REQUEST_TOTAL: std::time::Duration = std::time::Duration::from_millis(500);
const MOCK_DRAIN_TOTAL: std::time::Duration = std::time::Duration::from_millis(250);
const MOCK_MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Read one request with a bounded idle timeout, a total deadline and an
/// optional stop signal. Success requires complete headers plus the full
/// advertised Content-Length body; any timeout, stop, EOF, deadline or size
/// overflow before completion is an error, so a partial request can never be
/// served as a successful scripted response.
fn read_request(
    stream: &mut std::net::TcpStream,
    stop: Option<&std::sync::atomic::AtomicBool>,
    deadline: std::time::Instant,
) -> Result<String, String> {
    stream
        .set_nonblocking(false)
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(MOCK_REQUEST_IDLE))
        .map_err(|error| error.to_string())?;
    let mut data = Vec::new();
    let mut buffer = [0; 4096];
    let reason;
    loop {
        if stop.is_some_and(|stop| stop.load(std::sync::atomic::Ordering::SeqCst)) {
            reason = "stopped";
            break;
        }
        if std::time::Instant::now() >= deadline {
            reason = "total deadline";
            break;
        }
        match stream.read(&mut buffer) {
            Ok(0) => {
                reason = "connection closed";
                break;
            }
            Ok(size) => {
                data.extend_from_slice(&buffer[..size]);
                match complete_request_length(&data) {
                    Ok(Some(needed)) if data.len() >= needed => {
                        return Ok(String::from_utf8_lossy(&data).into_owned());
                    }
                    Ok(_) => {}
                    Err(error) => return Err(error),
                }
                if data.len() >= MOCK_MAX_REQUEST_BYTES {
                    reason = "size limit";
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                reason = "idle timeout";
                break;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    match complete_request_length(&data) {
        Ok(Some(needed)) if data.len() >= needed => {
            return Ok(String::from_utf8_lossy(&data).into_owned());
        }
        Ok(_) => {}
        Err(error) => return Err(error),
    }
    if data.is_empty() {
        return Err(format!("mock request {reason}"));
    }
    Err(format!(
        "mock request incomplete ({reason}); bytes={}",
        data.len()
    ))
}

/// The total byte length of a complete request, once its headers are present.
/// A Content-Length field is recognized by an exact case-insensitive field
/// name after splitting on the first colon; its value may have surrounding
/// ASCII whitespace and must be decimal digits that fit `usize`. Only an
/// absent field means a zero-length body. Invalid, overflowing or conflicting
/// framing is an explicit error, never an absent header. Arithmetic is
/// checked. This stays a minimal request parser, not an HTTP stack.
fn complete_request_length(data: &[u8]) -> Result<Option<usize>, String> {
    let Some(end) = data.windows(4).position(|part| part == b"\r\n\r\n") else {
        return Ok(None);
    };
    let headers = String::from_utf8_lossy(&data[..end]);
    let mut content_length = None;
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(format!(
                "unsupported Transfer-Encoding framing '{}'; this mock only accepts \
                 Content-Length or a zero-length body",
                value.trim_matches(|character: char| character.is_ascii_whitespace())
            ));
        }
        if !name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        let value = value.trim_matches(|character: char| character.is_ascii_whitespace());
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("invalid Content-Length value '{value}'"));
        }
        let length = value
            .parse::<usize>()
            .map_err(|_| format!("Content-Length does not fit usize: '{value}'"))?;
        if let Some(existing) = content_length
            && existing != length
        {
            return Err(format!(
                "conflicting Content-Length values ({existing} and {length})"
            ));
        }
        content_length = Some(length);
    }
    let length = content_length.unwrap_or(0);
    end.checked_add(4)
        .and_then(|headers_end| headers_end.checked_add(length))
        .map(Some)
        .ok_or_else(|| "Content-Length overflows the request size".to_owned())
}

/// Drain every connection already queued when the observer stops, so an
/// attempted request observed at teardown is still counted. One shared
/// deadline bounds the whole drain; exhausting it records a nonempty overflow
/// marker so a continuously replenished queue can neither prolong finish nor
/// look like a clean zero count.
fn drain_requests(
    listener: &TcpListener,
    requests: &mut Vec<String>,
    deadline: std::time::Instant,
) {
    while let Ok((mut stream, _)) = listener.accept() {
        if std::time::Instant::now() >= deadline {
            requests.push("<mock drain overflow: deadline exhausted>".to_owned());
            break;
        }
        match read_request(&mut stream, None, deadline) {
            Ok(data) => requests.push(data),
            Err(error) => requests.push(format!("<{error}>")),
        }
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

/// Add exactly one origin remote with an arbitrary supported transport so the
/// origin-attestation rules can be exercised without a second HTTP server.
fn set_origin_url(dir: &Path, url: &str) {
    use crate::git_runner::GitRunner;
    crate::git_runner::ProcessGitRunner::in_directory(dir)
        .run(&["remote", "add", "origin", url])
        .unwrap();
}

/// A second physical checkout sharing the fixture store, initialized like the
/// fixture repository but with no origin unless one is supplied.
fn checkout(label: &str, origin: Option<&str>) -> TempDir {
    let directory = TempDir::new(label);
    init_repo(directory.path());
    if let Some(origin) = origin {
        set_origin_url(directory.path(), origin);
    }
    directory
}

/// Resolve a checkout's repository key the same way production routing does.
fn repo_key_at(dir: &Path) -> String {
    use crate::git_runner::ProcessGitRunner;
    let runner = ProcessGitRunner::in_directory(dir);
    crate::branch_links::resolve_repo_key(None, &crate::branch_links::checkout_root(&runner))
        .unwrap()
        .key
}

/// The origin-derived key production uses for a checkout that has an origin.
fn checkout_repo_key(dir: &Path) -> String {
    use crate::git_runner::ProcessGitRunner;
    let runner = ProcessGitRunner::in_directory(dir);
    crate::branch_links::resolve_checkout_repo_key(&runner).unwrap()
}

/// One isolated fixture. Fields drop in declaration order, so the environment
/// guards restore the process state before the temporary directories are
/// removed: a test that reads the process-wide store without holding the
/// workflow lock can then never observe a path that has just been deleted.
struct Fixture {
    _db_env: EnvGuard,
    _index_env: EnvGuard,
    _local_env: EnvGuard,
    repo: TempDir,
    db: TempDir,
    _index: TempDir,
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
        registry::ensure_schema(&storage).unwrap();
        storage
            .save_credential(Role::Orchestrator, "forgejo", "fixture-token")
            .unwrap();
        storage
    }

    /// A store whose orchestrator role trusts the fixture mock instance. The
    /// placeholder is replaced by the first mock started for this fixture; the
    /// real CLI still resolves the exact stored instance and repository.
    fn attested_storage(&self) -> Storage {
        let storage = self.storage();
        storage
            .save_forgejo_auth(
                Role::Orchestrator,
                FIXTURE_UNATTESTED_INSTANCE,
                "owner/repo",
                "fixture-token",
            )
            .unwrap();
        storage
    }

    /// Replace this fixture's own placeholder trusted instance with the mock
    /// endpoint. Only this fixture's exact store is opened; a token-only or
    /// explicitly configured store is never rewritten.
    fn attest_endpoint(&self, base: &str) {
        let storage = Storage::open_at(&self.db.path().join("leases.sqlite3")).unwrap();
        let Some(config) = storage.load_forgejo_config(Role::Orchestrator).unwrap() else {
            return;
        };
        if config.api_base != FIXTURE_UNATTESTED_INSTANCE {
            return;
        }
        storage
            .save_forgejo_auth(
                Role::Orchestrator,
                base,
                &config.repository,
                "fixture-token",
            )
            .unwrap();
    }

    fn mock(&self, responses: Vec<String>) -> Mock {
        let mock = Mock::start(responses);
        self.attest_endpoint(&mock.base);
        mock
    }

    fn mock_status(&self, responses: Vec<(u16, String)>) -> Mock {
        let mock = Mock::start_status(responses);
        self.attest_endpoint(&mock.base);
        mock
    }

    fn zero_mock(&self) -> ZeroMock {
        let mock = ZeroMock::start();
        self.attest_endpoint(&mock.base);
        mock
    }

    fn identity(&self) -> String {
        repo_identity(&ProcessWorktreeRunner::new(), self.repo.path()).unwrap()
    }

    /// Give the fixture checkout an origin that attests the mock instance and
    /// names the repository, so implicit per-operation selection can be tested.
    fn set_origin(&self, base: &str, repository: &str) {
        let host = base.strip_prefix("http://").unwrap_or(base);
        set_origin_url(self.repo.path(), &format!("http://{host}/{repository}"));
    }

    #[allow(clippy::too_many_arguments)]
    fn lease(
        &self,
        storage: &Storage,
        _lease_id: &str,
        issue: u64,
        worktree: &Path,
        branch: &str,
        _status: &str,
        scope: Option<&WorktreeScope>,
    ) {
        registry::ensure_schema(storage).expect("registry schema");
        let identity = self.identity();
        let canonical = crate::worktree::canonical_or_self(worktree);
        let association = match scope {
            Some(scope) => NewAssociation::Legacy {
                issue,
                provider: scope.provider().to_owned(),
                project: scope.project().to_owned(),
                external_id: scope.external_id(),
            },
            None => NewAssociation::Unscoped { issue },
        };
        let existing = registry::record_for_path(storage, &identity, &canonical).expect("record");
        match existing {
            Some(record) => registry::attach_association(
                storage,
                &record.directory_id,
                &identity,
                1,
                "session-fixture",
                &association,
            )
            .expect("attach association"),
            None => {
                let directory_id = stable_directory_id(&identity, &canonical.to_string_lossy());
                registry::insert_directory(
                    storage,
                    &NewDirectory {
                        directory_id: &directory_id,
                        repo_identity: &identity,
                        checkout_path: self.repo.path().to_str().unwrap(),
                        worktree_path: &canonical.to_string_lossy(),
                        branch,
                        created_at: 1,
                        session: "session-fixture",
                        associations: &[association],
                    },
                )
                .expect("insert directory");
            }
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
        let storage = Storage::open_at(&self.db.path().join("leases.sqlite3")).unwrap();
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
    let storage = fixture.attested_storage();
    let mock = fixture.zero_mock();
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
fn acquire_under_an_implicit_forgejo_default_books_the_scoped_directory() {
    let fixture = Fixture::new("acquire-implicit-review");
    let storage = fixture.attested_storage();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    fixture.set_origin(&mock.base, "owner/repo");
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
    assert_eq!(exit, 0, "an implicit Forgejo default is admitted");
    assert!(requests.is_empty(), "acquire is local: {requests:?}");
    assert_eq!(
        rows.len(),
        1,
        "the acquire registers one directory: {rows:?}"
    );
    let record = &rows[0];
    assert_eq!(record.issue_numbers(), vec![7]);
    assert!(record.sessions.contains(&"fixture-session".to_owned()));
    let scope = record
        .legacy_issues
        .first()
        .expect("the default-resolved Forgejo acquire records its scope");
    assert_eq!(scope.provider, "forgejo");
    assert_eq!(scope.project, "owner/repo");
    assert_eq!(scope.external_id, 7);
    assert!(record.tasks.is_empty());
}

#[test]
fn stored_forgejo_default_without_owned_config_keeps_legacy_history() {
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
    let args: Vec<String> = ["worktree", "prune", "--repo", &repo, "--format", "json"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let prune_exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let after = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_ne!(
        acquire_exit, 0,
        "a Forgejo default without owned configuration fails on provenance"
    );
    assert_eq!(
        prune_exit, 0,
        "prune keeps unknown history instead of failing"
    );
    assert!(requests.is_empty());
    assert_eq!(after.len(), before.len());
    assert_eq!(
        after[0], before[0],
        "prune never rewrites registry evidence"
    );
    assert!(directory.exists());
}

#[test]
fn forgejo_close_override_is_rejected_before_existing_taxi_candidate() {
    let fixture = Fixture::new("close-override-taxi-review");
    let storage = fixture.attested_storage();
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
    assert_eq!(
        after[0], before[0],
        "a rejected override never rewrites evidence"
    );
    assert!(directory.exists());
}

#[cfg(unix)]
#[test]
fn global_prune_reports_all_skipped_repository_paths() {
    let fixture = Fixture::new("prune-skipped-mixed");
    let storage = fixture.storage();
    let _cache = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        fixture.db.path().join("cache").to_str().unwrap(),
    );
    let root = fixture.db.path().join("missing-main");
    let identity = root.join(".git").to_string_lossy().into_owned();
    let absent = fixture.db.path().join("absent");
    let existing = fixture.db.path().join("existing");
    let symlink = fixture.db.path().join("dangling");
    std::fs::create_dir(&existing).unwrap();
    std::os::unix::fs::symlink(fixture.db.path().join("missing-target"), &symlink).unwrap();
    for (id, path) in [
        ("absent", &absent),
        ("existing", &existing),
        ("symlink", &symlink),
        ("main", &root),
    ] {
        registry::insert_directory(
            &storage,
            &NewDirectory {
                directory_id: id,
                repo_identity: &identity,
                checkout_path: root.to_str().unwrap(),
                worktree_path: path.to_str().unwrap(),
                branch: "fixture",
                created_at: 1,
                session: "cli-fixture",
                associations: &[NewAssociation::Unscoped { issue: 7 }],
            },
        )
        .unwrap();
    }
    let args: Vec<String> = ["--output", "json", "worktree", "prune"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (exit, stdout, stderr) = run_captured(fixture.repo.path(), &args);
    assert_eq!(exit, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    // The absent main registration is a stale row like any other: it is
    // reclaimed, while the existing and the symlinked path stay protected.
    assert_eq!(value["totals"]["absent"], 2);
    assert_eq!(value["totals"]["kept_protected"], 2);
    let repo = &value["repos"][0];
    assert!(repo["skipped_reason"].is_string());
    assert_eq!(repo["worktrees"]["scanned"], 4);
    let actions = repo["worktrees"]["actions"].as_array().unwrap();
    for (id, code) in [
        ("existing", "repository_unavailable"),
        ("symlink", "path_probe_error"),
    ] {
        let action = actions
            .iter()
            .find(|action| action["directory_id"] == id)
            .unwrap();
        assert_eq!(action["result"], "skipped_protected");
        assert_eq!(action["reason_code"], code);
    }
    for id in ["absent", "main"] {
        let action = actions
            .iter()
            .find(|action| action["directory_id"] == id)
            .unwrap();
        assert_eq!(action["result"], "absent");
    }
    let remaining = registry::load_records(&storage, Some(&identity)).unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(existing.exists());
    assert!(std::fs::symlink_metadata(&symlink).is_ok());
}

#[cfg(unix)]
#[test]
fn forgejo_prune_reclaims_absent_unscoped_registration() {
    let fixture = Fixture::new("absent-unscoped-review");
    let storage = fixture.attested_storage();
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
    let mock = fixture.zero_mock();
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
    let (exit, stdout, _stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(exit, 0);
    assert!(requests.is_empty());
    assert!(rows.is_empty());
    // The absence is a fact of the disk: the report names the absent result
    // and never attributes the recorded history to the selected provider.
    assert!(
        stdout.contains("\"result\":\"absent\""),
        "the missing path is reported absent: {stdout}"
    );
    assert!(
        !stdout.contains("does not own unscoped legacy history"),
        "the report never blames the provider for the recorded history: {stdout}"
    );
    assert!(
        !stdout.contains("\"result\":\"skipped_issue\""),
        "the absence is not an issue-state keep: {stdout}"
    );
    assert!(
        !stdout.contains("\"result\":\"skipped_protected\""),
        "the absence is not an occupancy keep: {stdout}"
    );
    assert!(
        list_all_for_repo(&storage, &fixture.identity())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn explicit_legacy_acquire_overrides_stored_forgejo_default_without_auth() {
    for provider in ["local", "redmine"] {
        let fixture = Fixture::new(&format!("stored-default-{provider}-review"));
        let storage = Storage::open_at(&fixture.db.path().join("leases.sqlite3")).unwrap();
        registry::ensure_schema(&storage).unwrap();
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
        assert_eq!(rows[0].issue_numbers(), vec![7]);
        assert!(rows[0].sessions.contains(&"fixture-session".to_owned()));
        assert!(
            rows[0]
                .legacy_issues
                .iter()
                .all(|issue| issue.provider.is_empty()),
            "an explicit legacy acquire records no guessed provider scope"
        );
    }
}

#[test]
fn implicit_forgejo_defaults_reject_acquire_before_existing_taxi_candidate() {
    for default in ["env", "stored"] {
        let fixture = Fixture::new(&format!("implicit-taxi-{default}-review"));
        let storage = fixture.attested_storage();
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
        let mock = fixture.zero_mock();
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
        assert_eq!(
            after[0], before[0],
            "a rejected acquire rewrites no evidence"
        );
        assert!(directory.exists());
    }
}

#[cfg(unix)]
#[test]
fn legacy_closes_recheck_other_issue_before_removing_shared_directory() {
    let fixture = Fixture::new("legacy-close-sequential-review");
    let storage = fixture.attested_storage();
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
        let scope = WorktreeScope::new("local", "default", id).unwrap();
        fixture.lease(
            &storage,
            &format!("legacy-{id}"),
            id,
            &directory,
            "phasegent/7-close-sequential",
            status,
            Some(&scope),
        );
    }
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
fn env_forgejo_default_is_admitted_across_entry_paths() {
    let fixture = Fixture::new("absent-default-admission-review");
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let counter = RouteCounter::start();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let commands: Vec<Vec<String>> = vec![
        vec![
            "worktree".to_owned(),
            "acquire".to_owned(),
            "--issue".to_owned(),
            "7".to_owned(),
            "--session".to_owned(),
            "fixture-session".to_owned(),
            "--no-sync".to_owned(),
        ],
        vec!["worktree".to_owned(), "list".to_owned()],
        vec![
            "worktree".to_owned(),
            "prune".to_owned(),
            "--repo".to_owned(),
            repo.clone(),
            "--dryrun".to_owned(),
        ],
    ];
    for command in &commands {
        let mut args = vec!["--api-base".to_owned(), counter.base.clone()];
        args.extend(command.iter().cloned());
        let (_exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert!(
            !stderr.contains("explicit --provider forgejo"),
            "{command:?} must not report the removed admission rule: {stderr}"
        );
    }
    let requests = counter.finish();
    assert_eq!(requests, 0, "no route may reach the network: {requests}");
    let storage = Storage::open_at(&fixture.db.path().join("leases.sqlite3")).unwrap();
    registry::ensure_schema(&storage).unwrap();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert!(
        rows.is_empty(),
        "no lease is written without a task: {rows:?}"
    );
}

#[test]
fn stored_forgejo_default_is_admitted_without_writing_a_lease() {
    let fixture = Fixture::new("stored-default-no-migration-review");
    let storage = fixture.storage();
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
        .unwrap();
    let mock = ZeroMock::start();
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
    assert_ne!(
        exit, 0,
        "a Forgejo default without owned configuration fails on provenance"
    );
    assert!(requests.is_empty());
    assert!(rows.is_empty(), "no lease is written: {rows:?}");
}

#[cfg(unix)]
#[test]
fn forgejo_prune_without_credential_reclaims_absent_legacy_registration() {
    let fixture = Fixture::new("absent-no-credential-review");
    let storage = Storage::open_at(&fixture.db.path().join("leases.sqlite3")).unwrap();
    registry::ensure_schema(&storage).unwrap();
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
    let _exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert!(requests.is_empty());
    assert!(rows.is_empty(), "absence requires no provider credential");
}

#[cfg(unix)]
#[test]
fn forgejo_prune_keeps_same_number_legacy_on_another_path() {
    let fixture = Fixture::new("prune-same-number-other-path-review");
    let storage = fixture.attested_storage();
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
    let mock = fixture.zero_mock();
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

/// A zero-request observer that records any connection while the command
/// runs, closes it without a response, and stops promptly from its own test.
struct ZeroMock {
    base: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    task: Option<JoinHandle<Vec<String>>>,
}

impl ZeroMock {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = stop.clone();
        let task = thread::spawn(move || {
            let mut requests = Vec::new();
            loop {
                if done.load(std::sync::atomic::Ordering::SeqCst) {
                    drain_requests(
                        &listener,
                        &mut requests,
                        std::time::Instant::now() + MOCK_DRAIN_TOTAL,
                    );
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        match read_request(
                            &mut stream,
                            Some(&done),
                            std::time::Instant::now() + MOCK_REQUEST_TOTAL,
                        ) {
                            Ok(data) => requests.push(data),
                            Err(error) => requests.push(format!("<{error}>")),
                        }
                        // A zero-request endpoint never answers: closing the
                        // stream gives the client an immediate failure, so a
                        // retry loop cannot deadlock on a missing response.
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if done.load(std::sync::atomic::Ordering::SeqCst) {
                            drain_requests(
                                &listener,
                                &mut requests,
                                std::time::Instant::now() + MOCK_DRAIN_TOTAL,
                            );
                            break;
                        }
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
            requests
        });
        Self {
            base,
            stop,
            task: Some(task),
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.task
            .take()
            .unwrap()
            .join()
            .expect("zero-request observer panicked")
    }
}

impl Drop for ZeroMock {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

#[test]
fn scripted_mock_rejects_incomplete_or_unsupported_requests() {
    let cases: [(&str, &[u8]); 3] = [
        (
            "incomplete header",
            b"GET / HTTP/1.1\r\nHost: localhost\r\n",
        ),
        (
            "incomplete body",
            b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length:5\r\n\r\nx",
        ),
        (
            "unsupported transfer encoding",
            b"POST / HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n",
        ),
    ];
    for (label, request) in cases {
        let mock = Mock::start(vec!["ok".to_owned()]);
        let address = mock.base.strip_prefix("http://").unwrap();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        stream.write_all(request).unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        let requests = mock.finish();
        assert!(
            response.is_empty() && !response.contains("200 OK"),
            "{label} received a scripted success: {response:?}; {requests:?}"
        );
    }
}

#[test]
fn zero_mock_teardown_is_bounded_for_a_trickling_request() {
    let mock = ZeroMock::start();
    let address = mock.base.strip_prefix("http://").unwrap();
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream.write_all(b"G").unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stop = stop.clone();
    let writer = thread::spawn(move || {
        while !writer_stop.load(std::sync::atomic::Ordering::SeqCst) {
            if stream.write_all(b"x").is_err() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(40));
        }
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let observer = thread::spawn(move || tx.send(mock.finish()).unwrap());
    let result = rx.recv_timeout(std::time::Duration::from_millis(800));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    writer.join().unwrap();
    observer.join().unwrap();
    assert!(
        result.is_ok(),
        "a trickling request prevented bounded teardown"
    );
    assert!(
        !result.unwrap().is_empty(),
        "the attempted request must be counted"
    );
}

#[test]
fn zero_mock_teardown_is_bounded_for_a_replenished_queue() {
    let mock = ZeroMock::start();
    let address = mock.base.strip_prefix("http://").unwrap().to_owned();
    let mut seed = std::net::TcpStream::connect(&address).unwrap();
    seed.write_all(b"G").unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stop = stop.clone();
    let writer_address = address.clone();
    let writer = thread::spawn(move || {
        while !writer_stop.load(std::sync::atomic::Ordering::SeqCst) {
            if let Ok(mut stream) = std::net::TcpStream::connect(&writer_address) {
                let _ = stream.write_all(b"G");
            }
            thread::yield_now();
        }
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let observer = thread::spawn(move || tx.send(mock.finish()).unwrap());
    let result = rx.recv_timeout(std::time::Duration::from_millis(800));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    writer.join().unwrap();
    observer.join().unwrap();
    drop(seed);
    assert!(
        result.is_ok(),
        "a continuously replenished queue prevented bounded teardown"
    );
    assert!(
        !result.unwrap().is_empty(),
        "observed connections must never look like a clean zero count"
    );
}

fn run_as(repo: &Path, role: &str, args: &[String]) -> i32 {
    in_dir(repo, || {
        crate::cli::run_with_role(args.to_vec(), Some(role))
    })
}

fn cli_binary() -> PathBuf {
    static BINARY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BINARY
        .get_or_init(|| {
            if let Some(path) = option_env!("CARGO_BIN_EXE_phasegent") {
                return PathBuf::from(path);
            }
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
            let output = std::process::Command::new(cargo)
                .args(["build", "--bin", "phasegent", "--message-format=json"])
                .output()
                .expect("cargo build --bin phasegent");
            assert!(
                output.status.success(),
                "cargo build --bin phasegent must produce the CLI binary: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .expect("Cargo messages must be UTF-8")
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .find_map(|message| {
                    (message["reason"] == "compiler-artifact"
                        && message["target"]["name"] == "phasegent"
                        && message["target"]["kind"]
                            .as_array()
                            .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin")))
                    .then(|| message["executable"].as_str().map(PathBuf::from))
                    .flatten()
                })
                .expect("Cargo must report the phasegent executable artifact")
        })
        .clone()
}

fn run_captured(repo: &Path, args: &[String]) -> (i32, String, String) {
    run_captured_as(repo, args, "orchestrator")
}

fn run_captured_as(repo: &Path, args: &[String], role: &str) -> (i32, String, String) {
    let broker = identity_broker::TestBroker::start(role, "forgejo-cli-test");
    let output = std::process::Command::new(cli_binary())
        .args(args)
        .current_dir(repo)
        .env_remove("PHASEGENT_ROLE")
        .env(identity_broker::BROKER_ADDR_ENV, broker.addr())
        .env(identity_broker::BROKER_TOKEN_ENV, broker.token())
        .env("PHASEGENT_SESSION_ID", broker.session())
        .env("OPENCODE_SESSION_ID", broker.session())
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
fn forgejo_env_default_without_the_flag_reads_the_issue() {
    let fixture = Fixture::new("env-default");
    fixture.attested_storage();
    let _env = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let mock = fixture.mock(vec![REPO_JSON.to_owned(), issue_json(7, "open", "[]")]);
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
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "an environment Forgejo default is admitted");
    assert_eq!(
        requests.len(),
        2,
        "the default reads the issue: {requests:?}"
    );
}

#[test]
fn forgejo_stored_default_without_the_flag_reads_the_issue() {
    let fixture = Fixture::new("stored-default");
    let storage = fixture.attested_storage();
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
        .unwrap();
    let mock = fixture.mock(vec![REPO_JSON.to_owned(), issue_json(7, "open", "[]")]);
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
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "a persisted Forgejo default is admitted");
    assert_eq!(
        requests.len(),
        2,
        "the default reads the issue: {requests:?}"
    );
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
    fixture.attested_storage();
    fixture.link(7, "owner/repo");
    let mock = fixture.mock(vec![
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
    fixture.attested_storage();
    fixture.link(7, "owner/repo");
    let mock = fixture.zero_mock();
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
    fixture.attested_storage();
    fixture.link(7, "owner/repo");
    let _env = EnvGuard::set("PHASEGENT_PROVIDER", "local");
    let args: Vec<String> = [
        "record",
        "create",
        "7",
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
    assert_rejected(fixture.repo.path(), "executor", &args, fixture.zero_mock());
}

/// The responses one native status transition produces: the repository,
/// the issue before the write, the dependency preflight of a close, the
/// issue read again before the write and the patched issue.
fn status_transition_responses(current_state: &str, target_state: &str) -> Vec<String> {
    let mut responses = vec![REPO_JSON.to_owned(), issue_json(7, current_state, "[]")];
    if target_state == "closed" {
        responses.push("[]".to_owned());
    }
    responses.push(issue_json(7, current_state, "[]"));
    responses.push(issue_json(7, target_state, "[]"));
    responses
}

#[test]
fn forgejo_status_list_matrix() {
    let fixture = Fixture::new("status-list");
    fixture.attested_storage();
    let mock = fixture.zero_mock();
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
        "status",
        "list",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "status list must succeed: {stderr}");
    assert!(
        requests.is_empty(),
        "the native status catalogue reads nothing: {requests:?}"
    );
    assert!(
        stdout.contains("Open") && stdout.contains("Closed"),
        "status list must name the two native states: {stdout}"
    );
}

#[test]
fn forgejo_status_set_refuses_an_unsupported_target_before_http() {
    let fixture = Fixture::new("status-set-invalid");
    fixture.attested_storage();
    let mock = fixture.zero_mock();
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
        "status",
        "set",
        "7",
        "--status",
        "Resolved",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "an unsupported target must be refused");
    assert!(
        requests.is_empty(),
        "the refusal happens before any request: {requests:?}"
    );
    let reported = format!("{stdout}{stderr}");
    assert!(
        reported.contains("Open") && reported.contains("Closed"),
        "the refusal must name the two native states: {reported}"
    );
}

#[test]
fn forgejo_status_next_matrix() {
    let fixture = Fixture::new("status-next");
    fixture.attested_storage();
    fixture.link(7, "owner/repo");
    let mock = fixture.mock(vec![REPO_JSON.to_owned(), issue_json(7, "open", "[]")]);
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
    assert_eq!(
        requests.len(),
        2,
        "status next reads the repository and the issue only: {requests:?}"
    );
    assert!(
        !requests.iter().any(|request| request.contains("/labels")),
        "status next reads no label catalogue: {requests:?}"
    );
}

#[test]
fn forgejo_status_set_matrix() {
    let fixture = Fixture::new("status-set");
    fixture.attested_storage();
    fixture.link(7, "owner/repo");
    let mock = fixture.mock(status_transition_responses("open", "closed"));
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
        "Closed",
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
    assert!(
        requests
            .iter()
            .any(|request| request.ends_with(r#"{"state":"closed"}"#)),
        "status set must patch the native closed state: {requests:?}"
    );
    assert!(
        !requests.iter().any(|request| request.starts_with("PUT ")),
        "status set never writes labels: {requests:?}"
    );
    assert!(
        !requests.iter().any(|request| request.contains("/labels")),
        "status set reads no label catalogue: {requests:?}"
    );
}

#[test]
fn forgejo_status_advance_matrix() {
    let fixture = Fixture::new("status-advance");
    fixture.attested_storage();
    fixture.link(7, "owner/repo");
    let mock = fixture.mock(status_transition_responses("closed", "open"));
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
        "Open",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "status advance must succeed: {requests:?}");
    assert!(
        requests
            .iter()
            .any(|request| request.ends_with(r#"{"state":"open"}"#)),
        "status advance must patch the native open state: {requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.contains("/dependencies")),
        "reopening has no dependency gate: {requests:?}"
    );
    assert!(
        !requests.iter().any(|request| request.starts_with("PUT ")),
        "status advance never writes labels: {requests:?}"
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

/// One registry directory bound to an exact durable task.
struct TaskDirectorySpec<'a> {
    issue: u64,
    provider: &'a str,
    project: &'a str,
    binding: &'a crate::task_identity::TaskBinding,
    path: &'a Path,
    branch: &'a str,
}

fn task_directory_at(fixture: &Fixture, storage: &Storage, spec: TaskDirectorySpec<'_>) {
    let TaskDirectorySpec {
        issue,
        provider,
        project,
        binding,
        path,
        branch,
    } = spec;
    let identity = fixture.identity();
    let canonical = crate::worktree::canonical_or_self(path);
    let association = NewAssociation::Task {
        task_id: binding.task_id(),
        issue,
        provider: provider.to_owned(),
        project: project.to_owned(),
        external_id: issue,
    };
    let existing = registry::record_for_path(storage, &identity, &canonical).expect("record");
    match existing {
        Some(record) => registry::attach_association(
            storage,
            &record.directory_id,
            &identity,
            1,
            "session-fixture",
            &association,
        )
        .expect("attach task association"),
        None => {
            let directory_id = stable_directory_id(&identity, &canonical.to_string_lossy());
            registry::insert_directory(
                storage,
                &NewDirectory {
                    directory_id: &directory_id,
                    repo_identity: &identity,
                    checkout_path: fixture.repo.path().to_str().unwrap(),
                    worktree_path: &canonical.to_string_lossy(),
                    branch,
                    created_at: 1,
                    session: "session-fixture",
                    associations: &[association],
                },
            )
            .expect("insert task directory");
        }
    }
}

fn task_directory(
    fixture: &Fixture,
    storage: &Storage,
    issue: u64,
    binding: &crate::task_identity::TaskBinding,
    branch: &str,
) -> PathBuf {
    let directory = fixture.worktree(branch);
    task_directory_at(
        fixture,
        storage,
        TaskDirectorySpec {
            issue,
            provider: "forgejo",
            project: "owner/repo",
            binding,
            path: &directory,
            branch,
        },
    );
    directory
}

fn scoped_lease(fixture: &Fixture, storage: &Storage, issue: u64, status: &str) -> PathBuf {
    let scope = WorktreeScope::new("forgejo", "owner/repo", issue).unwrap();
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
fn production_prune_with_explicit_forgejo_context_cleans_the_scoped_worktree() {
    let fixture = Fixture::new("prune-explicit");
    let storage = fixture.attested_storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = fixture.mock(vec![
        REPO_JSON.to_owned(),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
        issue_json(7, "closed", "[]"),
    ]);
    let base = mock.base.clone();
    fixture.set_origin(&base, "owner/repo");
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
fn production_prune_reports_missing_role_configuration_with_a_safe_hint() {
    let fixture = Fixture::new("prune-missing-config-hint");
    let storage = fixture.storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--output", "json", "worktree", "prune", "--repo", &repo, "--dryrun",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, stdout, stderr) = run_captured(fixture.repo.path(), &args);
    assert_eq!(exit, 0, "{stderr}");
    let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let action = &result["repos"][0]["worktrees"]["actions"][0];
    assert_eq!(
        action["reason_code"], "closure_missing_configuration",
        "{stdout}"
    );
    assert!(
        action["message"]
            .as_str()
            .unwrap()
            .contains("Configure the tracking provider")
    );
    let mut text_args = args.clone();
    text_args[1] = "text".to_owned();
    let (exit, text, stderr) = run_captured(fixture.repo.path(), &text_args);
    assert_eq!(exit, 0, "{stderr}");
    assert!(
        text.contains("hint: Configure the tracking provider and credential for this role."),
        "{text}"
    );
    assert!(!text.contains("fixture-token"));
    assert!(directory.exists());
}

#[cfg(unix)]
#[test]
fn production_prune_classifies_bound_incomplete_configuration_before_provider_reads() {
    for case in [
        "redmine-missing",
        "redmine-empty",
        "gitlab-incomplete",
        "gitlab-empty-project",
        "gitlab-empty-base",
    ] {
        let fixture = Fixture::new(&format!("prune-{case}"));
        let storage = fixture.storage();
        crate::branch_links::ensure_schema(&storage.connection).unwrap();
        let mock = ZeroMock::start();
        let (provider, project) = if case.starts_with("redmine") {
            ("redmine", "23")
        } else {
            ("gitlab", "owner/repo")
        };
        let key = crate::task_identity::TaskKey::new(provider, &mock.base, project, "7").unwrap();
        let binding = crate::task_identity::ensure_binding(&storage, &key, 1).unwrap();
        crate::task_identity::add_repo_membership(
            &storage,
            &repo_key_at(fixture.repo.path()),
            binding.task_id(),
            1,
        )
        .unwrap();
        if provider == "redmine" {
            storage
                .save_global_setting("PHASEGENT_REDMINE_API_BASE", &mock.base)
                .unwrap();
            if case == "redmine-empty" {
                storage.connection.execute("INSERT INTO role_credential (role, provider, credential) VALUES ('orchestrator', 'redmine', '')", []).unwrap();
            }
        } else {
            storage
                .save_gitlab_auth(Role::Orchestrator, &mock.base, project, "fixture-secret")
                .unwrap();
            let update = match case {
                "gitlab-empty-project" => {
                    "UPDATE role_gitlab_config SET repository = '' WHERE role = 'orchestrator'"
                }
                "gitlab-empty-base" => {
                    "UPDATE role_gitlab_config SET api_base = '' WHERE role = 'orchestrator'"
                }
                _ => "UPDATE role_gitlab_config SET repository = NULL WHERE role = 'orchestrator'",
            };
            storage.connection.execute(update, []).unwrap();
        }
        let directory = fixture.worktree("phasegent/bound-config");
        task_directory_at(
            &fixture,
            &storage,
            TaskDirectorySpec {
                issue: 7,
                provider,
                project,
                binding: &binding,
                path: &directory,
                branch: "phasegent/bound-config",
            },
        );
        let (_stub, _path) = opencode_stub();
        let _unset = UnsetEnv::apply(&[
            "PHASEGENT_REDMINE_API_BASE",
            "PHASEGENT_PROVIDER",
            "PHASEGENT_DEFAULT_PROVIDER",
        ]);
        let repo = fixture.repo.path().to_string_lossy().to_string();
        let args: Vec<String> = [
            "--output", "json", "worktree", "prune", "--repo", &repo, "--dryrun",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let (exit, stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_eq!(exit, 0, "{case}: {stderr}");
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            result["repos"][0]["worktrees"]["actions"][0]["reason_code"],
            "closure_missing_configuration",
            "{case}: {stdout}"
        );
        assert!(!stdout.contains("fixture-secret"));
        assert!(directory.exists());
        assert!(
            mock.finish().is_empty(),
            "{case}: unexpected remote request"
        );
    }
}

#[cfg(unix)]
#[test]
fn production_prune_reports_denied_provider_access_without_response_body() {
    let fixture = Fixture::new("prune-denied-hint");
    let storage = fixture.attested_storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = Mock::start_status(vec![
        (200, REPO_JSON.to_owned()),
        (403, "secret-provider-body".to_owned()),
    ]);
    let base = mock.base.clone();
    fixture.attest_endpoint(&base);
    fixture.set_origin(&base, "owner/repo");
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--output",
        "json",
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
        "--dryrun",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, stdout, stderr) = run_captured(fixture.repo.path(), &args);
    assert_eq!(exit, 0, "{stderr}");
    let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        result["repos"][0]["worktrees"]["actions"][0]["reason_code"],
        "closure_authentication_denied",
        "{stdout}"
    );
    assert!(!stdout.contains("secret-provider-body"));
    assert!(!stdout.contains("fixture-token"));
    assert!(directory.exists());
    assert!(!mock.finish().is_empty());
}

#[cfg(unix)]
#[test]
fn production_prune_foreign_and_default_contexts_keep_the_scoped_worktree() {
    for (label, extra) in [
        (
            "foreign",
            vec!["--provider", "forgejo", "--repository", "other/repo"],
        ),
        (
            "foreign-origin",
            vec!["--provider", "forgejo", "--repository", "owner/repo"],
        ),
        ("default", vec![]),
    ] {
        let fixture = Fixture::new(&format!("prune-{label}"));
        let storage = fixture.attested_storage();
        fixture.link(7, "owner/repo");
        let directory = scoped_lease(&fixture, &storage, 7, "retained");
        let (_stub, _path) = opencode_stub();
        let mock = fixture.zero_mock();
        let base = mock.base.clone();
        if label == "foreign-origin" {
            fixture.set_origin(&base, "other/repo");
        }
        let repo = fixture.repo.path().to_string_lossy().to_string();
        let mut args: Vec<String> = extra.iter().map(|value| (*value).to_owned()).collect();
        if label != "default" {
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
    let storage = fixture.attested_storage();
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
    let storage = fixture.attested_storage();
    fixture.link(7, "owner/repo");
    let directory = scoped_lease(&fixture, &storage, 7, "retained");
    let (_stub, _path) = opencode_stub();
    let mock = fixture.zero_mock();
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

#[test]
fn production_prune_explicit_redmine_never_uses_forgejo_default() {
    let fixture = Fixture::new("prune-redmine-fg-default");
    let storage = fixture.attested_storage();
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
    drop(_default);
    drop(storage);
    drop(fixture);

    let fixture = Fixture::new("prune-redmine-endpoint-conflict");
    let storage = fixture.attested_storage();
    crate::branch_links::ensure_schema(&storage.connection).unwrap();
    let directory = fixture.worktree("phasegent/7-redmine-conflict");
    let scope = WorktreeScope::new("redmine", "23", 7).unwrap();
    fixture.lease(
        &storage,
        "redmine-conflict",
        7,
        &directory,
        "phasegent/7-redmine-conflict",
        "retained",
        Some(&scope),
    );
    let trusted = ZeroMock::start();
    let foreign = ZeroMock::start();
    storage
        .save_global_setting("PHASEGENT_REDMINE_API_BASE", &trusted.base)
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-key")
        .unwrap();
    let _unset = UnsetEnv::apply(&["PHASEGENT_REDMINE_API_BASE"]);
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "redmine",
        "--project-id",
        "23",
        "--api-base",
        &foreign.base,
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
    let (exit, stdout, stderr) = run_captured(fixture.repo.path(), &args);
    assert_eq!(exit, 0, "preview: {stderr}");
    let result: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(result["totals"]["kept_issue"], 1, "{result}");
    assert_eq!(
        result["repos"][0]["worktrees"]["actions"][0]["reason_code"], "closure_scope_mismatch",
        "{stdout}"
    );
    assert!(directory.exists());
    assert!(
        trusted.finish().is_empty(),
        "trusted endpoint must not be read"
    );
    assert!(
        foreign.finish().is_empty(),
        "foreign endpoint must not receive credentials"
    );
}

#[test]
fn forgejo_create_binding_rejects_a_later_omitted_provider() {
    let fixture = Fixture::new("create-binding-omitted");
    fixture.attested_storage();
    use crate::git_runner::GitRunner;
    crate::git_runner::ProcessGitRunner::in_directory(fixture.repo.path())
        .run(&[
            "remote",
            "add",
            "origin",
            "https://example.test/owner/repo.git",
        ])
        .unwrap();
    let mock = fixture.mock(vec![REPO_JSON.to_owned(), issue_json(7, "open", "[]")]);
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
    let get_mock = fixture.zero_mock();
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

/// Two fixture providers with different answers: the explicit Local source
/// says issue 9 is open while the default Forgejo fixture says closed. Only
/// the explicit target may authorize cleanup, and the default is never read.
#[test]
fn legacy_close_closure_uses_only_the_explicit_provider_endpoint() {
    let fixture = Fixture::new("legacy-close-two-providers");
    let storage = fixture.attested_storage();
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
    // Real historical provenance: a unique local/default link proves which
    // provider owns number 9. The explicit selection only constrains that
    // proven scope; it is not ownership by itself.
    crate::branch_links::ensure_schema(&storage.connection).unwrap();
    let runner = crate::git_runner::ProcessGitRunner::in_directory(fixture.repo.path());
    let branch = crate::git_runner::current_branch(&runner).unwrap();
    let repo_key = route_repo_key(&fixture);
    let issue_key = crate::branch_links::IssueKey::from_number("local", "default", 9).unwrap();
    crate::branch_links::store::link(
        &storage.connection,
        &crate::branch_links::LinkParams {
            repo_key: &repo_key,
            branch: &branch,
            issue: &issue_key,
            issue_number: 9,
            source: "fixture",
            now: 10,
        },
    )
    .unwrap();
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
        let storage = fixture.attested_storage();
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
        for (id, status) in [(7, "active"), (9, "retained")] {
            let scope = WorktreeScope::new("local", "default", id).unwrap();
            fixture.lease(
                &storage,
                &format!("legacy-{id}"),
                id,
                &directory,
                "phasegent/7-legacy-close",
                status,
                Some(&scope),
            );
        }
        let (_stub, _path) = opencode_stub();
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
fn stored_forgejo_default_without_owned_config_does_not_mutate_during_taxi_list() {
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
    let args: Vec<String> = ["worktree", "list"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    assert_eq!(exit, 0, "the listing is admitted under the Forgejo default");
    assert_eq!(
        connection
            .query_row(
                "SELECT value FROM global_setting WHERE name='PHASEGENT_DEFAULT_PROVIDER'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "forgejo",
        "the listing never rewrites the stored default"
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
fn default_forgejo_store_states_never_corrupt_or_request() {
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
        let _provider = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
        let parent = if state == "inaccessible" {
            let dir = fixture.db.path().join("blocked");
            std::fs::create_dir(&dir).unwrap();
            dir
        } else {
            fixture.db.path().to_path_buf()
        };
        let path = parent.join("leases.sqlite3");
        let _db = EnvGuard::set("PHASEGENT_DB_PATH", path.to_str().unwrap());
        let mut storage = None;
        let mut initialized_record = None;
        if state == "old" || state == "initialized" {
            if state == "old" {
                let connection = rusqlite::Connection::open(&path).unwrap();
                connection
                    .execute_batch(
                        "CREATE TABLE global_setting (name TEXT PRIMARY KEY, value TEXT); \
                         INSERT INTO global_setting VALUES ('PHASEGENT_DEFAULT_PROVIDER','forgejo');",
                    )
                    .unwrap();
            } else {
                let opened = Storage::open_at(&path).unwrap();
                registry::ensure_schema(&opened).unwrap();
                opened
                    .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
                    .unwrap();
                fixture.link(8, "owner/repo");
                let directory = scoped_lease(&fixture, &opened, 8, "retained");
                assert!(directory.exists());
                initialized_record =
                    Some(list_all_for_repo(&opened, &fixture.identity()).unwrap()[0].clone());
            }
            storage = Storage::open_at(&path).ok();
        }
        let parent_mode = fixture_permissions(&parent);
        let counter = RouteCounter::start();
        let mut args = vec![
            "--api-base".to_owned(),
            counter.base.clone(),
            "worktree".to_owned(),
            "acquire".to_owned(),
            "--issue".to_owned(),
            "7".to_owned(),
            "--session".to_owned(),
            "fixture-session".to_owned(),
            "--no-sync".to_owned(),
        ];
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
            assert_eq!(_blocked_mode.1.unwrap() & 0o777, 0, "{state}");
        }
        assert_ne!(exit, 0, "{state}");
        let expected = if state == "inaccessible" {
            "could not inspect phasegent database"
        } else {
            "Forgejo"
        };
        assert!(stderr.contains(expected), "{state}: {stderr}");
        assert!(
            !stderr.contains("explicit --provider forgejo"),
            "{state}: {stderr}"
        );
        assert_eq!(requests, 0, "{state}");
        #[cfg(unix)]
        if state == "inaccessible" {
            assert_eq!(fixture_permissions(&parent), parent_mode, "{state}");
        }
        if let Some(opened) = storage.as_ref() {
            registry::ensure_schema(opened).unwrap();
            let rows = list_all_for_repo(opened, &fixture.identity()).unwrap();
            if state == "initialized" {
                assert_eq!(rows.len(), 1, "{state}: {rows:?}");
                assert_eq!(
                    Some(rows[0].clone()),
                    initialized_record,
                    "{state}: a read-only listing must not touch directory evidence"
                );
            } else {
                assert!(rows.is_empty(), "{state}: {rows:?}");
            }
        } else if path.exists() {
            // The common selection path opens the store before it proves the
            // provider scope or credential, so the file may exist; it must
            // still hold no lease because the route failed.
            let opened = Storage::open_at(&path).unwrap();
            registry::ensure_schema(&opened).unwrap();
            let rows = list_all_for_repo(&opened, &fixture.identity()).unwrap();
            assert!(rows.is_empty(), "{state}: {rows:?}");
        }
        args.clear();
    }
}

#[test]
fn old_stored_default_is_admitted_across_every_entry_route() {
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
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let counter = RouteCounter::start();
    let commands: Vec<Vec<String>> = vec![
        vec![
            "worktree".to_owned(),
            "acquire".to_owned(),
            "--issue".to_owned(),
            "7".to_owned(),
            "--session".to_owned(),
            "fixture-session".to_owned(),
            "--no-sync".to_owned(),
        ],
        vec!["worktree".to_owned(), "list".to_owned()],
        vec![
            "worktree".to_owned(),
            "prune".to_owned(),
            "--repo".to_owned(),
            repo.clone(),
            "--dryrun".to_owned(),
        ],
    ];
    for command in &commands {
        let mut args = vec!["--api-base".to_owned(), counter.base.clone()];
        args.extend(command.iter().cloned());
        let (_exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert!(
            !stderr.contains("explicit --provider forgejo"),
            "route {command:?} must not report the removed admission rule: {stderr}"
        );
    }
    let requests = counter.finish();
    assert_eq!(requests, 0, "no route may reach the network: {requests}");
    assert_eq!(
        connection
            .query_row(
                "SELECT value FROM global_setting WHERE name='PHASEGENT_DEFAULT_PROVIDER'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "forgejo",
        "no route changed stored default data"
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
        vec!["worktree", "prune", "--repo", &repo, "--dryrun"],
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

fn route_repo_key(fixture: &Fixture) -> String {
    repo_key_at(fixture.repo.path())
}

fn bind_task(
    storage: &Storage,
    fixture: &Fixture,
    instance: &str,
    number: u64,
) -> crate::task_identity::TaskBinding {
    bind_task_in_repo(
        storage,
        &route_repo_key(fixture),
        "forgejo",
        instance,
        "owner/repo",
        number,
    )
}

/// Bind one durable task to an explicit physical repository key.
fn bind_task_in_repo(
    storage: &Storage,
    repo_key: &str,
    provider: &str,
    instance: &str,
    project: &str,
    number: u64,
) -> crate::task_identity::TaskBinding {
    let key = crate::task_identity::TaskKey::new(provider, instance, project, &number.to_string())
        .unwrap();
    let binding = crate::task_identity::ensure_binding(storage, &key, 10).unwrap();
    crate::task_identity::add_repo_membership(storage, repo_key, binding.task_id(), 10).unwrap();
    binding
}

/// Every timer ledger row as `(run_id, issue, status)`.
fn timer_rows(fixture: &Fixture) -> Vec<(String, u64, String)> {
    let storage =
        Storage::open_at(&fixture.db.path().join("leases.sqlite3")).expect("timer store opens");
    let mut statement = storage
        .connection
        .prepare("SELECT run_id, issue_id, status FROM execution_timer_runs ORDER BY started_at")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

/// The complete ledger row of one run, so a refusal can be compared on every
/// field rather than only on status.
fn timer_run_snapshot(fixture: &Fixture, run_id: &str) -> serde_json::Value {
    let storage =
        Storage::open_at(&fixture.db.path().join("leases.sqlite3")).expect("timer store opens");
    let run = storage
        .load_timer_run(run_id)
        .expect("timer row reads")
        .expect("timer row exists");
    serde_json::to_value(run).expect("timer row serialises")
}

/// One ledger row read from a fresh connection.
fn load_timer_run(fixture: &Fixture, run_id: &str) -> crate::infra::storage::TimerRun {
    let storage =
        Storage::open_at(&fixture.db.path().join("leases.sqlite3")).expect("timer store opens");
    storage
        .load_timer_run(run_id)
        .expect("timer row reads")
        .expect("timer row exists")
}

/// Put a terminal run into a projection claim so the stale/live recovery
/// branches can be exercised deterministically.
fn mark_projection_claim(fixture: &Fixture, run_id: &str, claimed_at: i64) {
    let storage =
        Storage::open_at(&fixture.db.path().join("leases.sqlite3")).expect("timer store opens");
    storage
        .connection
        .execute(
            "UPDATE execution_timer_runs SET sync_status = 'projecting', \
             projection_token = 'fixture-token', projection_claimed_at = ?2 WHERE run_id = ?1",
            rusqlite::params![run_id, claimed_at],
        )
        .expect("projection claim is written");
}

#[test]
fn timer_start_refuses_a_bound_forgejo_task_before_the_ledger_write() {
    let fixture = Fixture::new("timer-bound-forgejo-start");
    let storage = fixture.attested_storage();
    let mock = fixture.zero_mock();
    bind_task(&storage, &fixture, &mock.base, 7);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "timer",
        "start",
        "7",
        "--phase",
        "P1",
        "--agent-role",
        "executor",
        "--attempt",
        "1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "a bound Forgejo task has no time tracking");
    assert!(
        requests.is_empty(),
        "the refusal must precede any request: {requests:?}"
    );
    assert!(
        timer_rows(&fixture).is_empty(),
        "the refusal must precede the ledger write"
    );
}

#[test]
fn timer_finish_refuses_a_bound_forgejo_task_before_the_transition() {
    let fixture = Fixture::new("timer-bound-forgejo-finish");
    let storage = fixture.attested_storage();
    let mock = fixture.zero_mock();
    bind_task(&storage, &fixture, &mock.base, 7);
    let run_id = "run-bound-forgejo";
    storage
        .start_timer_run_with_owner(
            run_id,
            7,
            "P1",
            "executor",
            1,
            crate::worktree::now_unix_secs().max(1),
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["timer", "finish", run_id, "--result", "DONE"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(exit, 0);
    assert!(requests.is_empty());
    assert_eq!(
        timer_rows(&fixture),
        vec![(run_id.to_owned(), 7, "running".to_owned())],
        "the refusal must leave the run running and unprojected"
    );
}

#[test]
fn timer_recover_refuses_a_bound_forgejo_task_before_the_transition() {
    let fixture = Fixture::new("timer-bound-forgejo-recover");
    let storage = fixture.attested_storage();
    let mock = fixture.zero_mock();
    bind_task(&storage, &fixture, &mock.base, 7);
    let run_id = "run-bound-forgejo-recover";
    storage
        .start_timer_run_with_owner(
            run_id,
            7,
            "P1",
            "executor",
            1,
            crate::worktree::now_unix_secs().max(1),
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let before = timer_run_snapshot(&fixture, run_id);
    let args: Vec<String> = ["timer", "recover", run_id]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_ne!(exit, 0);
    assert!(
        stderr.contains("not_supported"),
        "the refusal must name the unsupported Forgejo time tracking: {stderr}"
    );
    assert!(requests.is_empty());
    assert_eq!(
        timer_rows(&fixture),
        vec![(run_id.to_owned(), 7, "running".to_owned())],
        "the refusal must leave the run running and unprojected"
    );
    assert_eq!(
        timer_run_snapshot(&fixture, run_id),
        before,
        "the refusal must leave every ledger field unchanged"
    );
}

#[test]
fn timer_recover_refuses_a_stale_projection_for_a_bound_forgejo_task() {
    let fixture = Fixture::new("timer-bound-forgejo-recover-stale");
    let storage = fixture.attested_storage();
    let mock = fixture.zero_mock();
    bind_task(&storage, &fixture, &mock.base, 7);
    let run_id = "run-bound-forgejo-recover-stale";
    let now = crate::worktree::now_unix_secs().max(1);
    storage
        .start_timer_run_with_owner(
            run_id,
            7,
            "P1",
            "executor",
            1,
            now,
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    storage.finish_timer_run(run_id, "DONE", now + 1).unwrap();
    mark_projection_claim(&fixture, run_id, 1);
    let before = timer_run_snapshot(&fixture, run_id);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["timer", "recover", run_id]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    let requests = mock.finish();
    assert_ne!(exit, 0);
    assert!(
        requests.is_empty(),
        "the refusal must precede any request: {requests:?}"
    );
    assert_eq!(
        timer_run_snapshot(&fixture, run_id),
        before,
        "a stale projection claim must not be reset for a bound Forgejo task"
    );
}

#[test]
fn timer_recover_bound_redmine_projects_to_the_pinned_endpoint_under_a_changed_default() {
    let fixture = Fixture::new("timer-bound-redmine-recover");
    let storage = fixture.attested_storage();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-redmine-key")
        .unwrap();
    let mock = PathMock::start(vec![
        path_rule(
            "GET",
            "/enumerations/time_entry_activities.json",
            r#"{"time_entry_activities":[{"id":9,"name":"AI automation","is_default":false}]}"#,
        ),
        path_rule(
            "GET",
            "/time_entries.json",
            r#"{"total_count":0,"limit":100,"time_entries":[]}"#,
        ),
        path_rule_fn("POST", "/time_entries.json", |_, _, _| {
            (
                201,
                r#"{"time_entry":{"id":77,"issue":{"id":8},"activity":{"id":9,"name":"AI automation"},"hours":0.01,"comments":"phasegent timer run_id=fixture","spent_on":"2026-11-06"}}"#.to_owned(),
            )
        }),
    ]);
    let base = mock.base.clone();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _api_base = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", &base);
    // The changed default is Local: the bound task must still be served by
    // its own pinned Redmine instance and project.
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "local");
    bind_task_in_repo(
        &storage,
        &route_repo_key(&fixture),
        "redmine",
        &base,
        "42",
        8,
    );
    let run_id = "run-bound-redmine-recover";
    storage
        .start_timer_run_with_owner(
            run_id,
            8,
            "P1",
            "executor",
            1,
            crate::worktree::now_unix_secs().max(1),
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    let args: Vec<String> = ["timer", "recover", run_id]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    let requests = mock.finish();
    assert_eq!(
        exit, 0,
        "the bound Redmine recover must stay usable: {requests:?}"
    );
    let post = requests
        .iter()
        .find(|(method, target, _)| {
            method == "POST" && target.split('?').next() == Some("/time_entries.json")
        })
        .expect("the time entry must reach the pinned endpoint");
    assert!(
        post.2.contains("\"issue_id\":8"),
        "the pinned issue must be projected: {}",
        post.2
    );
    let run = load_timer_run(&fixture, run_id);
    assert_eq!(run.status, "FAILED");
    assert_eq!(
        (run.time_entry_id, run.sync_status.as_str()),
        (Some(77), "synced")
    );
}

#[test]
fn timer_recover_bound_local_stays_local_without_a_remote_projection() {
    let fixture = Fixture::new("timer-bound-local-recover");
    let storage = fixture.attested_storage();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-redmine-key")
        .unwrap();
    // Any remote projection would reach this listener; the bound Local task
    // must never do that.
    let mock = fixture.zero_mock();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _api_base = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", &mock.base);
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "redmine");
    bind_task_in_repo(
        &storage,
        &route_repo_key(&fixture),
        "local",
        "local",
        "default",
        8,
    );
    let run_id = "run-bound-local-recover";
    storage
        .start_timer_run_with_owner(
            run_id,
            8,
            "P1",
            "executor",
            1,
            crate::worktree::now_unix_secs().max(1),
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    let args: Vec<String> = ["timer", "recover", run_id]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    let requests = mock.finish();
    assert_eq!(exit, 0, "the bound Local recover must stay usable");
    assert!(
        requests.is_empty(),
        "a bound Local run must never project remotely: {requests:?}"
    );
    let run = load_timer_run(&fixture, run_id);
    assert_eq!(
        (
            run.status.as_str(),
            run.time_entry_id,
            run.sync_status.as_str()
        ),
        ("FAILED", None, "failed"),
        "the Local run stays a local failed ledger row"
    );
}

#[test]
fn timer_recover_refuses_an_ambiguous_current_repository_task() {
    let fixture = Fixture::new("timer-recover-ambiguous");
    let storage = fixture.attested_storage();
    let repo = route_repo_key(&fixture);
    bind_task_in_repo(
        &storage,
        &repo,
        "redmine",
        "https://redmine-a.test",
        "23",
        8,
    );
    bind_task_in_repo(&storage, &repo, "local", "local", "default", 8);
    let run_id = "run-recover-ambiguous";
    storage
        .start_timer_run_with_owner(
            run_id,
            8,
            "P1",
            "executor",
            1,
            crate::worktree::now_unix_secs().max(1),
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    let before = timer_run_snapshot(&fixture, run_id);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["timer", "recover", run_id]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    assert_ne!(exit, 0, "an ambiguous current-repository task must refuse");
    assert_eq!(
        timer_run_snapshot(&fixture, run_id),
        before,
        "the ambiguous refusal must leave the ledger unchanged"
    );
}

#[test]
fn timer_recover_refuses_conflicting_redmine_instances_before_the_transition() {
    let fixture = Fixture::new("timer-recover-redmine-conflict");
    let storage = fixture.attested_storage();
    bind_task_in_repo(
        &storage,
        &route_repo_key(&fixture),
        "redmine",
        "https://redmine-a.test",
        "23",
        8,
    );
    // A second Redmine scope for the same number cannot be attributed by a
    // ledger that stores no task id, so recovery refuses before any write.
    bind_task_in_repo(
        &storage,
        "foreign:repo",
        "redmine",
        "https://redmine-b.test",
        "23",
        8,
    );
    let run_id = "run-recover-redmine-conflict";
    storage
        .start_timer_run_with_owner(
            run_id,
            8,
            "P1",
            "executor",
            1,
            crate::worktree::now_unix_secs().max(1),
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    let before = timer_run_snapshot(&fixture, run_id);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["timer", "recover", run_id]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    assert_ne!(exit, 0, "conflicting Redmine scopes must refuse");
    assert_eq!(
        timer_run_snapshot(&fixture, run_id),
        before,
        "the conflicting-scope refusal must leave the ledger unchanged"
    );
}

#[test]
fn timer_recover_refuses_a_changed_endpoint_before_the_transition() {
    let fixture = Fixture::new("timer-recover-changed-endpoint");
    let storage = fixture.attested_storage();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-redmine-key")
        .unwrap();
    bind_task_in_repo(
        &storage,
        &route_repo_key(&fixture),
        "redmine",
        "https://redmine-old.test",
        "23",
        8,
    );
    let run_id = "run-recover-changed-endpoint";
    storage
        .start_timer_run_with_owner(
            run_id,
            8,
            "P1",
            "executor",
            1,
            crate::worktree::now_unix_secs().max(1),
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    let before = timer_run_snapshot(&fixture, run_id);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    // The configured endpoint moved away from the bound task instance: the
    // credential can no longer attest it, so recovery refuses before any
    // transition or projection.
    let _api_base = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", "https://redmine-new.test");
    let args: Vec<String> = ["timer", "recover", run_id]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    assert_ne!(exit, 0);
    assert_eq!(timer_run_snapshot(&fixture, run_id), before);
}

#[test]
fn timer_recover_legacy_run_still_fails_locally_without_provider_config() {
    let fixture = Fixture::new("timer-recover-legacy-missing-config");
    let storage = fixture.attested_storage();
    let run_id = "run-recover-legacy";
    storage
        .start_timer_run_with_owner(
            run_id,
            11,
            "P1",
            "executor",
            1,
            crate::worktree::now_unix_secs().max(1),
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _unset = UnsetEnv::apply(&[
        "PHASEGENT_PROVIDER",
        "PHASEGENT_DEFAULT_PROVIDER",
        "PHASEGENT_REDMINE_API_BASE",
        "PHASEGENT_API_BASE",
    ]);
    let args: Vec<String> = ["timer", "recover", run_id]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    assert_ne!(
        exit, 0,
        "the projection cannot be configured, so the command reports it"
    );
    assert_eq!(
        load_timer_run(&fixture, run_id).status,
        "FAILED",
        "the durable local FAILED transition is preserved for an unbound legacy run"
    );
}

#[test]
fn timer_recover_does_not_clear_a_live_projection_claim() {
    let fixture = Fixture::new("timer-recover-live-claim");
    let storage = fixture.attested_storage();
    let run_id = "run-recover-live-claim";
    let now = crate::worktree::now_unix_secs().max(1);
    storage
        .start_timer_run_with_owner(
            run_id,
            11,
            "P1",
            "executor",
            1,
            now,
            &crate::infra::storage::TimerRunOwner::default(),
        )
        .unwrap();
    storage.finish_timer_run(run_id, "DONE", now + 1).unwrap();
    mark_projection_claim(&fixture, run_id, now);
    let before = timer_run_snapshot(&fixture, run_id);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "local");
    let args: Vec<String> = ["timer", "recover", run_id]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    assert_ne!(exit, 0, "a live projection claim is not stealable");
    assert_eq!(
        timer_run_snapshot(&fixture, run_id),
        before,
        "a live projection claim and its token must stay unchanged"
    );
}

/// Issue 1201 P10 N1: a task bound in this repository owns the acquisition
/// for every provider with the flags omitted, so a changed or implicit
/// default never rejects or re-points it.
#[test]
fn bound_task_acquire_without_provider_uses_the_exact_task_scope() {
    for (label, provider, instance, project) in [
        ("redmine", "redmine", "https://redmine-a.test", "23"),
        ("local", "local", "local", "default"),
        ("forgejo", "forgejo", "https://forgejo.test", "owner/repo"),
    ] {
        let fixture = Fixture::new(&format!("acquire-bound-{label}"));
        let storage = fixture.attested_storage();
        let _config = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            fixture.db.path().join("missing.toml").to_str().unwrap(),
        );
        let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        // The bound task must survive a changed default, and an implicit
        // Forgejo default must no longer reject it before routing.
        let _default = match provider {
            "redmine" => Some(EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "local")),
            "local" => Some(EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "redmine")),
            _ => Some(EnvGuard::set("PHASEGENT_PROVIDER", "forgejo")),
        };
        let binding = bind_task_in_repo(
            &storage,
            &route_repo_key(&fixture),
            provider,
            instance,
            project,
            7,
        );
        let mock = fixture.zero_mock();
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
        let (exit, stdout, stderr) = run_captured(fixture.repo.path(), &args);
        let requests = mock.finish();
        assert_eq!(exit, 0, "{label}: {stderr}");
        assert!(requests.is_empty(), "{label}: {}", requests.len());
        let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        assert_eq!(rows.len(), 1, "{label}: {rows:?}");
        assert_eq!(rows[0].issue_numbers(), vec![7], "{label}");
        let task = rows[0]
            .tasks
            .first()
            .unwrap_or_else(|| panic!("{label}: no durable task association: {rows:?}"));
        assert_eq!(task.task_id, binding.task_id(), "{label}");
        assert_eq!(
            (
                task.provider.as_deref(),
                task.project.as_deref(),
                task.external_id.as_deref(),
            ),
            (Some(provider), Some(project), Some("7")),
            "{label}"
        );
        assert_eq!(task.instance.as_deref(), Some(instance), "{label}");
        assert!(
            rows[0].sessions.contains(&"fixture-session".to_owned()),
            "{label}"
        );
        assert!(
            stdout.contains("\"repo_identity\""),
            "{label}: envelope keeps the identity: {stdout}"
        );
    }
}

/// Issue 1201 P10 N2: an ambiguous current-repository task refuses before
/// taxi reconciliation or any lease/store write, even when the taxi has a
/// candidate the ambient default provider would contact.
#[test]
fn bound_task_acquire_refuses_ambiguity_before_taxi_and_writes() {
    let fixture = Fixture::new("acquire-bound-ambiguous");
    let storage = fixture.attested_storage();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let repo = route_repo_key(&fixture);
    bind_task_in_repo(
        &storage,
        &repo,
        "redmine",
        "https://redmine-a.test",
        "23",
        7,
    );
    bind_task_in_repo(&storage, &repo, "local", "local", "default", 7);
    let directory = fixture.worktree("phasegent/8-taxi");
    fixture.lease(
        &storage,
        "legacy-eight",
        8,
        &directory,
        "phasegent/8-taxi",
        "retained",
        None,
    );
    let mock = fixture.zero_mock();
    // A default provider and endpoint the taxi would have to contact for its
    // retained candidate if it ran before admission.
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "redmine");
    let _api = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", &mock.base);
    let before = complete_fixture_snapshot(&storage.connection);
    for no_sync in [false, true] {
        let mut args: Vec<String> = [
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
        if no_sync {
            args.push("--no-sync".to_owned());
        }
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_ne!(exit, 0, "{stderr}");
        assert!(stderr.contains("multiple durable tasks"), "{stderr}");
        assert_eq!(
            complete_fixture_snapshot(&storage.connection),
            before,
            "refusal must preserve all tables and schema with no_sync={no_sync}"
        );
        assert!(directory.exists());
    }
    let requests = mock.finish();
    assert!(
        requests.is_empty(),
        "taxi ran before admission: {requests:?}"
    );
}

/// Issue 1201 P10 N2: an explicit selection that contradicts the bound task
/// refuses before any write instead of falling back to a legacy scope.
/// Issue 1201 P10 N2: every malformed raw close-status value refuses for a
/// bound task and for a legacy number, before taxi reconciliation or any
/// lease/store write; a valid value keeps the bound-task routing and a
/// bound Forgejo task keeps the provider prohibition.
/// Issue 1201 P10 N2: a blank raw provider selection refuses before taxi or
/// any lease/store write, for a bound task and for a legacy number alike, and
/// a valid project selection keeps the legacy acquisition.
#[test]
fn acquire_rejects_blank_provider_selections_before_writes() {
    // A bound task with a blank project selection.
    {
        let fixture = Fixture::new("acquire-blank-project-bound");
        let storage = fixture.attested_storage();
        let _config = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            fixture.db.path().join("missing.toml").to_str().unwrap(),
        );
        let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        bind_task_in_repo(
            &storage,
            &route_repo_key(&fixture),
            "redmine",
            "https://redmine-a.test",
            "23",
            7,
        );
        let args: Vec<String> = [
            "--provider",
            "redmine",
            "--project-id",
            "   ",
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
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_ne!(exit, 0, "{stderr}");
        assert!(stderr.contains("project id cannot be empty"), "{stderr}");
        assert!(
            list_all_for_repo(&storage, &fixture.identity())
                .unwrap()
                .is_empty()
        );
    }

    // A legacy number with a blank project selection keeps its own refusal.
    {
        let fixture = Fixture::new("acquire-blank-project-legacy");
        let storage = fixture.attested_storage();
        let _config = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            fixture.db.path().join("missing.toml").to_str().unwrap(),
        );
        let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        let args: Vec<String> = [
            "--provider",
            "redmine",
            "--project-id",
            "   ",
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
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_ne!(exit, 0, "blank project accepted: {stderr}");
        assert!(stderr.contains("project id cannot be empty"), "{stderr}");
        assert!(
            list_all_for_repo(&storage, &fixture.identity())
                .unwrap()
                .is_empty(),
            "a rejected legacy invocation must not write a lease"
        );
    }

    // A legacy number with a blank endpoint or repository selection.
    for (label, selection) in [
        ("api-base", vec!["--api-base", "   "]),
        ("repository", vec!["--repository", "   "]),
    ] {
        let fixture = Fixture::new(&format!("acquire-blank-{label}-legacy"));
        let storage = fixture.attested_storage();
        let _config = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            fixture.db.path().join("missing.toml").to_str().unwrap(),
        );
        let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        let mut args: Vec<String> = selection.iter().map(|value| (*value).to_owned()).collect();
        args.extend(
            [
                "worktree",
                "acquire",
                "--issue",
                "7",
                "--session",
                "fixture-session",
                "--no-sync",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_ne!(exit, 0, "{label}: {stderr}");
        assert!(stderr.contains("cannot be empty"), "{label}: {stderr}");
        assert!(
            list_all_for_repo(&storage, &fixture.identity())
                .unwrap()
                .is_empty(),
            "{label}: a rejected invocation must not write a lease"
        );
    }

    // A valid project selection keeps the legacy acquisition.
    {
        let fixture = Fixture::new("acquire-valid-project-legacy");
        let storage = fixture.attested_storage();
        let _config = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            fixture.db.path().join("missing.toml").to_str().unwrap(),
        );
        let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        let args: Vec<String> = [
            "--provider",
            "redmine",
            "--project-id",
            "42",
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
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_eq!(exit, 0, "{stderr}");
        let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].tasks.is_empty(),
            "a legacy number keeps the legacy numeric association"
        );
    }
}

#[test]
fn acquire_rejects_malformed_close_status_before_taxi_and_writes() {
    for (label, bound, value) in [
        ("bound-nonnumeric", true, "abc"),
        ("bound-zero", true, "0"),
        ("bound-overflow", true, "999999999999999999999"),
        ("legacy-zero", false, "0"),
    ] {
        let fixture = Fixture::new(&format!("acquire-malformed-close-{label}"));
        let storage = fixture.attested_storage();
        let _config = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            fixture.db.path().join("missing.toml").to_str().unwrap(),
        );
        let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        if bound {
            bind_task_in_repo(
                &storage,
                &route_repo_key(&fixture),
                "redmine",
                "https://redmine-a.test",
                "23",
                7,
            );
        }
        let directory = fixture.worktree(&format!("phasegent/8-{label}"));
        fixture.lease(
            &storage,
            &format!("legacy-eight-{label}"),
            8,
            &directory,
            &format!("phasegent/8-{label}"),
            "retained",
            None,
        );
        let mock = fixture.zero_mock();
        // The taxi candidate would be read through this default provider if
        // the malformed flag were not rejected first.
        let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "redmine");
        let _api = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", &mock.base);
        let before = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        let args: Vec<String> = [
            "--close-status-id",
            value,
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
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        let requests = mock.finish();
        assert_ne!(exit, 0, "{label}: {stderr}");
        assert!(stderr.contains("close status id"), "{label}: {stderr}");
        assert!(requests.is_empty(), "{label}: taxi ran: {requests:?}");
        assert_eq!(
            list_all_for_repo(&storage, &fixture.identity()).unwrap(),
            before,
            "{label}: registry rows changed"
        );
        assert!(directory.exists());
    }

    // A valid close-status id does not disturb the bound-task acquisition.
    {
        let fixture = Fixture::new("acquire-valid-close");
        let storage = fixture.attested_storage();
        let _config = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            fixture.db.path().join("missing.toml").to_str().unwrap(),
        );
        let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        let repo = route_repo_key(&fixture);
        let binding = bind_task_in_repo(
            &storage,
            &repo,
            "redmine",
            "https://redmine-a.test",
            "23",
            7,
        );
        let args: Vec<String> = [
            "--close-status-id",
            "5",
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
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_eq!(exit, 0, "{stderr}");
        let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].tasks.first().map(|task| task.task_id),
            Some(binding.task_id()),
            "a valid close status must keep the task-owned directory"
        );
    }

    // The Forgejo prohibition on the flag itself is unchanged.
    {
        let fixture = Fixture::new("acquire-valid-close-forgejo");
        let storage = fixture.attested_storage();
        let _config = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            fixture.db.path().join("missing.toml").to_str().unwrap(),
        );
        let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
        bind_task_in_repo(
            &storage,
            &route_repo_key(&fixture),
            "forgejo",
            "https://forgejo.test",
            "owner/repo",
            7,
        );
        let args: Vec<String> = [
            "--close-status-id",
            "5",
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
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_ne!(exit, 0, "{stderr}");
        assert!(
            stderr.contains("does not support --project-id or --close-status-id"),
            "{stderr}"
        );
        assert!(
            list_all_for_repo(&storage, &fixture.identity())
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn bound_task_acquire_refuses_contradicting_selection_before_any_write() {
    let fixture = Fixture::new("acquire-bound-contradictions");
    let storage = fixture.attested_storage();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let repo = route_repo_key(&fixture);
    bind_task_in_repo(
        &storage,
        &repo,
        "redmine",
        "https://redmine-a.test",
        "23",
        7,
    );
    let mock = fixture.zero_mock();
    let cases: [&[&str]; 4] = [
        &["--provider", "local"],
        &["--provider", "forgejo", "--repository", "owner/repo"],
        &["--api-base", "https://redmine-other.test"],
        &["--project-id", "99"],
    ];
    for selection in cases {
        let mut args: Vec<String> = selection.iter().map(|value| (*value).to_owned()).collect();
        args.extend(
            [
                "worktree",
                "acquire",
                "--issue",
                "7",
                "--session",
                "fixture-session",
                "--no-sync",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
        assert_ne!(exit, 0, "{selection:?}: {stderr}");
        assert!(
            stderr.contains("does not match durable task"),
            "{selection:?}: {stderr}"
        );
    }
    let requests = mock.finish();
    assert!(requests.is_empty(), "{requests:?}");
    assert!(
        list_all_for_repo(&storage, &fixture.identity())
            .unwrap()
            .is_empty()
    );
}

/// Issue 1201 P10 N2: an orphan branch task reference refuses before taxi,
/// HTTP or any lease write.
#[test]
fn bound_task_acquire_refuses_an_orphan_branch_reference() {
    use crate::git_runner::ProcessGitRunner;
    let fixture = Fixture::new("acquire-bound-orphan");
    let storage = fixture.attested_storage();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    crate::branch_links::ensure_schema(&storage.connection).unwrap();
    let runner = ProcessGitRunner::in_directory(fixture.repo.path());
    let branch = crate::git_runner::current_branch(&runner).unwrap();
    let repo_key = route_repo_key(&fixture);
    let issue = crate::branch_links::IssueKey::from_number("forgejo", "owner/repo", 7).unwrap();
    crate::branch_links::store::link_many_with_task(
        &storage.connection,
        &[(
            crate::branch_links::LinkParams {
                repo_key: &repo_key,
                branch: &branch,
                issue: &issue,
                issue_number: 7,
                source: "fixture",
                now: 10,
            },
            Some(9999),
        )],
    )
    .unwrap();
    let mock = fixture.zero_mock();
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
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "{stderr}");
    assert!(stderr.contains("no durable task binding"), "{stderr}");
    assert!(requests.is_empty(), "{requests:?}");
    assert!(
        list_all_for_repo(&storage, &fixture.identity())
            .unwrap()
            .is_empty()
    );
}

/// Issue 1201 P10 N3: same-number tasks on two instances stay distinct, an
/// explicit selection disambiguates them, and the acquisition never reuses the
/// other instance's lease.
#[test]
fn bound_task_acquire_selects_one_instance_and_never_reuses_the_other() {
    let fixture = Fixture::new("acquire-bound-two-instances");
    let storage = fixture.attested_storage();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let repo = route_repo_key(&fixture);
    let task_a = bind_task_in_repo(
        &storage,
        &repo,
        "forgejo",
        "https://a.test",
        "owner/repo",
        7,
    );
    let task_b = bind_task_in_repo(
        &storage,
        &repo,
        "forgejo",
        "https://b.test",
        "owner/repo",
        7,
    );
    let _other = task_directory(&fixture, &storage, 7, &task_b, "phasegent/7-b");
    let before = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let unfiltered: Vec<String> = [
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
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &unfiltered);
    assert_ne!(
        exit, 0,
        "two instances must refuse without a selection: {stderr}"
    );
    assert_eq!(
        list_all_for_repo(&storage, &fixture.identity()).unwrap(),
        before
    );
    let selected: Vec<String> = [
        "--api-base",
        "https://a.test",
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
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &selected);
    assert_eq!(exit, 0, "{stderr}");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    let acquired = rows
        .iter()
        .find(|row| {
            !before
                .iter()
                .any(|old| old.directory_id == row.directory_id)
        })
        .unwrap_or_else(|| panic!("no acquired directory: {rows:?}"));
    assert_eq!(rows.len(), before.len() + 1);
    let task = acquired.tasks.first().expect("task association");
    assert_eq!(task.task_id, task_a.task_id());
    assert_eq!(task.instance.as_deref(), Some("https://a.test"));
    let other = rows
        .iter()
        .find(|row| row.directory_id == before[0].directory_id)
        .unwrap();
    assert_eq!(
        other, &before[0],
        "the other instance's directory must stay untouched"
    );
}

/// Issue 1201 P10 N1: only a membership of this repository owns an
/// acquisition; a same-number task recorded in another repository keeps the
/// legacy explicit-Forgejo scope instead of being guessed over.
#[test]
fn foreign_task_membership_does_not_own_a_local_acquire() {
    let fixture = Fixture::new("acquire-foreign-task");
    let storage = fixture.attested_storage();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _clean = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    let mock = fixture.zero_mock();
    bind_task_in_repo(
        &storage,
        "foreign:repo",
        "forgejo",
        &mock.base,
        "owner/repo",
        7,
    );
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "--repository",
        "owner/repo",
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
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "{stderr}");
    assert!(requests.is_empty(), "{requests:?}");
    let rows = list_all_for_repo(&storage, &fixture.identity()).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].tasks.is_empty(),
        "a foreign membership must not own this acquisition"
    );
    let scope = rows[0]
        .legacy_issues
        .first()
        .expect("the explicit Forgejo scope is recorded");
    assert_eq!(
        (
            scope.provider.as_str(),
            scope.project.as_str(),
            scope.external_id
        ),
        ("forgejo", "owner/repo", 7)
    );
}

#[test]
fn foreign_repo_forgejo_task_does_not_block_a_legacy_timer() {
    let fixture = Fixture::new("timer-foreign-forgejo");
    let storage = fixture.attested_storage();
    // The task belongs to another physical repository, so it must never
    // decide this number's timer route.
    bind_task_in_repo(
        &storage,
        "foreign:repo",
        "forgejo",
        "https://forgejo.test",
        "owner/repo",
        7,
    );
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "local");
    let args: Vec<String> = [
        "timer",
        "start",
        "7",
        "--phase",
        "P1",
        "--agent-role",
        "executor",
        "--attempt",
        "1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    assert_eq!(
        exit, 0,
        "a foreign-repository task must not block the supported legacy timer"
    );
    assert_eq!(
        timer_rows(&fixture),
        vec![(timer_rows(&fixture)[0].0.clone(), 7, "running".to_owned())]
    );
}

#[test]
fn timer_refuses_an_ambiguous_current_repository_task() {
    let fixture = Fixture::new("timer-current-ambiguous");
    let storage = fixture.attested_storage();
    let repo = route_repo_key(&fixture);
    bind_task_in_repo(
        &storage,
        &repo,
        "redmine",
        "https://redmine-a.test",
        "23",
        7,
    );
    bind_task_in_repo(&storage, &repo, "local", "local", "default", 7);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "timer",
        "start",
        "7",
        "--phase",
        "P1",
        "--agent-role",
        "executor",
        "--attempt",
        "1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    assert_ne!(exit, 0, "an ambiguous current-repository task must refuse");
    assert!(timer_rows(&fixture).is_empty());
}

#[test]
fn timer_refuses_conflicting_redmine_instances_before_the_ledger_write() {
    let fixture = Fixture::new("timer-redmine-conflict");
    let storage = fixture.attested_storage();
    bind_task_in_repo(
        &storage,
        &route_repo_key(&fixture),
        "redmine",
        "https://redmine-a.test",
        "23",
        7,
    );
    // The same number on a second Redmine instance cannot be attributed by a
    // ledger that stores no task id, so the read-only conflict check refuses
    // instead of selecting an endpoint.
    bind_task_in_repo(
        &storage,
        "foreign:repo",
        "redmine",
        "https://redmine-b.test",
        "23",
        7,
    );
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "timer",
        "start",
        "7",
        "--phase",
        "P1",
        "--agent-role",
        "executor",
        "--attempt",
        "1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    assert_ne!(
        exit, 0,
        "conflicting Redmine instances must refuse before the ledger write"
    );
    assert!(timer_rows(&fixture).is_empty());
}

#[test]
fn bound_redmine_timer_projects_to_the_pinned_endpoint_under_a_changed_default() {
    let fixture = Fixture::new("timer-bound-redmine");
    let storage = fixture.attested_storage();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-redmine-key")
        .unwrap();
    let mock = PathMock::start(vec![
        path_rule(
            "GET",
            "/enumerations/time_entry_activities.json",
            r#"{"time_entry_activities":[{"id":9,"name":"AI automation","is_default":false}]}"#,
        ),
        path_rule(
            "GET",
            "/time_entries.json",
            r#"{"total_count":0,"limit":100,"time_entries":[]}"#,
        ),
        path_rule_fn("POST", "/time_entries.json", |_, _, _| {
            (
                201,
                r#"{"time_entry":{"id":77,"issue":{"id":7},"activity":{"id":9,"name":"AI automation"},"hours":0.01,"comments":"phasegent timer run_id=fixture","spent_on":"2026-11-06"}}"#.to_owned(),
            )
        }),
    ]);
    let base = mock.base.clone();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _api_base = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", &base);
    // The changed default is Local: the bound task must still be served by
    // its own pinned Redmine instance and project.
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "local");
    bind_task_in_repo(
        &storage,
        &route_repo_key(&fixture),
        "redmine",
        &base,
        "42",
        7,
    );
    let start: Vec<String> = [
        "timer",
        "start",
        "7",
        "--phase",
        "P1",
        "--agent-role",
        "executor",
        "--attempt",
        "1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_captured(fixture.repo.path(), &start).0;
    assert_eq!(exit, 0, "the bound Redmine timer start must stay usable");
    let run_id = timer_rows(&fixture)[0].0.clone();
    let finish: Vec<String> = ["timer", "finish", &run_id, "--result", "DONE"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &finish).0;
    let requests = mock.finish();
    assert_eq!(
        exit, 0,
        "the bound Redmine timer finish must stay usable: {requests:?}"
    );
    let post = requests
        .iter()
        .find(|(method, target, _)| {
            method == "POST" && target.split('?').next() == Some("/time_entries.json")
        })
        .expect("the time entry must reach the pinned endpoint");
    assert!(
        post.2.contains("\"issue_id\":7"),
        "the pinned issue must be projected: {}",
        post.2
    );
    assert_eq!(
        timer_rows(&fixture)[0].2,
        "DONE",
        "the run must finish locally"
    );
    let storage =
        Storage::open_at(&fixture.db.path().join("leases.sqlite3")).expect("timer store opens");
    let run = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(
        (run.time_entry_id, run.sync_status.as_str()),
        (Some(77), "synced"),
        "the pinned endpoint must accept the projection"
    );
}

#[test]
fn bound_local_timer_stays_local_without_a_remote_projection() {
    let fixture = Fixture::new("timer-bound-local");
    let storage = fixture.attested_storage();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-redmine-key")
        .unwrap();
    // Any remote projection would reach this listener; the bound Local task
    // must never do that.
    let mock = fixture.zero_mock();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _api_base = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", &mock.base);
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "redmine");
    bind_task_in_repo(
        &storage,
        &route_repo_key(&fixture),
        "local",
        "local",
        "default",
        7,
    );
    let start: Vec<String> = [
        "timer",
        "start",
        "7",
        "--phase",
        "P1",
        "--agent-role",
        "executor",
        "--attempt",
        "1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        run_captured(fixture.repo.path(), &start).0,
        0,
        "a bound Local timer start must stay usable"
    );
    let run_id = timer_rows(&fixture)[0].0.clone();
    let finish: Vec<String> = ["timer", "finish", &run_id, "--result", "DONE"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_captured(fixture.repo.path(), &finish).0;
    let requests = mock.finish();
    assert_eq!(exit, 0, "a bound Local timer finish must stay usable");
    assert!(
        requests.is_empty(),
        "a bound Local task must never project remotely: {requests:?}"
    );
    let storage =
        Storage::open_at(&fixture.db.path().join("leases.sqlite3")).expect("timer store opens");
    let run = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(
        (run.status.as_str(), run.time_entry_id),
        ("DONE", None),
        "the Local run stays a local ledger row"
    );
}

#[test]
fn bound_redmine_timer_refuses_a_changed_endpoint_before_the_ledger_write() {
    let fixture = Fixture::new("timer-redmine-changed-endpoint");
    let storage = fixture.attested_storage();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-redmine-key")
        .unwrap();
    bind_task_in_repo(
        &storage,
        &route_repo_key(&fixture),
        "redmine",
        "https://redmine-old.test",
        "23",
        7,
    );
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    // The configured endpoint moved away from the bound task instance: the
    // role credential can no longer attest that instance, so the timer
    // refuses instead of projecting to the changed default.
    let _api_base = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", "https://redmine-new.test");
    let args: Vec<String> = [
        "timer",
        "start",
        "7",
        "--phase",
        "P1",
        "--agent-role",
        "executor",
        "--attempt",
        "1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_captured(fixture.repo.path(), &args).0;
    assert_ne!(exit, 0);
    assert!(timer_rows(&fixture).is_empty());
}

#[test]
fn bound_only_batch_succeeds_with_an_implicit_forgejo_default() {
    let fixture = Fixture::new("batch-bound-only");
    let storage = fixture.attested_storage();
    let mock = PathMock::start(vec![
        path_rule("GET", "/api/v1/repos/owner/repo", REPO_JSON),
        path_rule(
            "GET",
            "/api/v1/repos/owner/repo/issues/9",
            &issue_json(9, "open", "[]"),
        ),
        path_rule(
            "GET",
            "/api/v1/repos/owner/repo/issues/10",
            &issue_json(10, "open", "[]"),
        ),
    ]);
    let base = mock.base.clone();
    storage
        .save_forgejo_auth(Role::Orchestrator, &base, "owner/repo", "fixture-token")
        .unwrap();
    bind_task(&storage, &fixture, &base, 9);
    bind_task(&storage, &fixture, &base, 10);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    // The Forgejo default is implicit (no --provider): a bound-only batch
    // must not construct that unproven default.
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
    let args: Vec<String> = ["issue", "get", "9", "10"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(
        exit, 0,
        "a bound-only batch must succeed with an implicit Forgejo default: {requests:?}"
    );
    assert!(requests_hit(
        &requests,
        "GET",
        "/api/v1/repos/owner/repo/issues/9"
    ));
    assert!(requests_hit(
        &requests,
        "GET",
        "/api/v1/repos/owner/repo/issues/10"
    ));
}

#[test]
fn batch_isolates_an_ambiguous_item_and_serves_the_bound_item() {
    let fixture = Fixture::new("batch-ambiguous-isolated");
    let storage = fixture.attested_storage();
    let mock = PathMock::start(vec![
        path_rule("GET", "/api/v1/repos/owner/repo", REPO_JSON),
        path_rule(
            "GET",
            "/api/v1/repos/owner/repo/issues/10",
            &issue_json(10, "open", "[]"),
        ),
    ]);
    let base = mock.base.clone();
    storage
        .save_forgejo_auth(Role::Orchestrator, &base, "owner/repo", "fixture-token")
        .unwrap();
    let repo = route_repo_key(&fixture);
    // Issue 9 carries two durable tasks in this repository, so it cannot be
    // attributed; issue 10 is uniquely bound.
    bind_task_in_repo(&storage, &repo, "forgejo", &base, "owner/repo", 9);
    bind_task_in_repo(&storage, &repo, "local", "local", "default", 9);
    bind_task(&storage, &fixture, &base, 10);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "local");
    let args: Vec<String> = ["issue", "get", "9", "10"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(
        exit, 1,
        "the ambiguous item is an isolated error while the bound item succeeds: {requests:?}"
    );
    assert!(requests_hit(
        &requests,
        "GET",
        "/api/v1/repos/owner/repo/issues/10"
    ));
    assert!(
        !requests
            .iter()
            .any(|(_, target, _)| target.contains("/issues/9")),
        "the ambiguous item must not be routed to any provider: {requests:?}"
    );
}

#[test]
fn batch_routes_a_bound_and_a_legacy_item_independently() {
    let fixture = Fixture::new("batch-bound-and-legacy");
    let storage = fixture.attested_storage();
    let mock = PathMock::start(vec![
        path_rule("GET", "/api/v1/repos/owner/repo", REPO_JSON),
        path_rule(
            "GET",
            "/api/v1/repos/owner/repo/issues/9",
            &issue_json(9, "open", "[]"),
        ),
    ]);
    let base = mock.base.clone();
    storage
        .save_forgejo_auth(Role::Orchestrator, &base, "owner/repo", "fixture-token")
        .unwrap();
    bind_task(&storage, &fixture, &base, 9);
    // The unbound item is served by the legacy Local path from the fixture
    // local store.
    let local_path = fixture.db.path().join("local.sqlite3");
    let local = crate::infra::local_store::open_local_at(&local_path).unwrap();
    local
        .connection
        .execute(
            "INSERT INTO local_issues (id,title,body,status,project,tracker,author_role,created_at,updated_at,closed_at) VALUES (11,'legacy','', 'New','default','Task','executor',1,1,NULL)",
            [],
        )
        .unwrap();
    let _local = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_path.to_string_lossy().as_ref(),
    );
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _default = EnvGuard::set("PHASEGENT_PROVIDER", "local");
    let args: Vec<String> = ["issue", "get", "9", "11"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(
        exit, 0,
        "the bound item and the legacy Local item must both succeed: {requests:?}"
    );
    assert!(requests_hit(
        &requests,
        "GET",
        "/api/v1/repos/owner/repo/issues/9"
    ));
    assert!(
        !requests
            .iter()
            .any(|(_, target, _)| target.contains("/issues/11")),
        "the legacy Local item must not reach the Forgejo instance: {requests:?}"
    );
}

#[test]
fn auto_timer_hooks_skip_a_bound_forgejo_task_before_any_ledger_write() {
    let fixture = Fixture::new("auto-timer-forgejo");
    let storage = fixture.attested_storage();
    bind_task(&storage, &fixture, "https://forgejo.test", 7);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    in_dir(fixture.repo.path(), || {
        // The lifecycle auto start refuses before opening a run.
        assert!(
            crate::time_tracking_cli::auto_start_run(7, "P1", "executor", 1).is_err(),
            "a bound Forgejo task has no time tracking"
        );
        // The lifecycle auto close skips without a ledger write.
        assert!(matches!(
            crate::lifecycle_auto::auto_close_issue_timer(7, crate::providers::ProviderKind::Local),
            crate::lifecycle_auto::AutoCloseOutcome::Noop { .. }
        ));
        // The auto finish refuses to close an existing run.
        let run_id = "auto-bound-forgejo";
        fixture
            .storage()
            .start_timer_run_with_owner(
                run_id,
                7,
                "P1",
                "executor",
                1,
                crate::worktree::now_unix_secs().max(1),
                &crate::infra::storage::TimerRunOwner::default(),
            )
            .unwrap();
        assert!(
            crate::time_tracking_cli::auto_finish_run(run_id).is_err(),
            "the auto finish must refuse a bound Forgejo task"
        );
        assert_eq!(
            timer_rows(&fixture),
            vec![(run_id.to_owned(), 7, "running".to_owned())],
            "the skipped auto hooks must leave the ledger unchanged"
        );
    });
}

#[test]
fn bound_forgejo_task_routes_get_after_default_switch() {
    let fixture = Fixture::new("route-default-switch");
    let storage = fixture.attested_storage();
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(9, "open", "[]")]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    bind_task(&storage, &fixture, &mock.base, 9);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "redmine");
    let args: Vec<String> = ["issue", "get", "9"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(
        exit, 0,
        "the bound Forgejo task must route despite the default"
    );
    assert!(
        requests.iter().any(|request| request.contains("/issues/9")),
        "the pinned instance must receive the get: {requests:?}"
    );
}

#[test]
fn an_associated_number_is_read_through_its_own_task_by_membership_only() {
    use crate::cli::issue_closure::{IssueStateSource, TaskScopeRef, configured_source_task_aware};

    let fixture = Fixture::new("closure-associated-task");
    let storage = fixture.attested_storage();
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(11, "closed", "[]")]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let eleven = bind_task(&storage, &fixture, &mock.base, 11);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "local");
    let source = configured_source_task_aware(
        Role::Orchestrator,
        crate::cli::provider_context::ProviderContext::default(),
    );
    // One membership: the number is read through its own pinned task.
    let verdict = source.associated_closure(fixture.repo.path(), 11);
    assert!(
        verdict.is_closed(),
        "the associated task answers its own closure: {verdict:?}"
    );
    let requests = mock.finish();
    assert!(
        requests
            .iter()
            .any(|request| request.contains("/issues/11")),
        "the associated task instance answers: {requests:?}"
    );

    // A second same-number task on an instance this role does not serve makes
    // the membership ambiguous: the number keeps and no further read happens.
    let other = Mock::start(Vec::new());
    let other_key =
        crate::task_identity::TaskKey::new("forgejo", &other.base, "owner/repo", "11").unwrap();
    let other_binding = crate::task_identity::ensure_binding(&storage, &other_key, 12).unwrap();
    crate::task_identity::add_repo_membership(
        &storage,
        &route_repo_key(&fixture),
        other_binding.task_id(),
        12,
    )
    .unwrap();
    source.invalidate(fixture.repo.path(), &[11]);
    let verdict = source.associated_closure(fixture.repo.path(), 11);
    assert!(
        verdict
            .keep_reason()
            .is_some_and(|reason| reason.contains("several durable tasks")),
        "ambiguous membership keeps without guessing: {verdict:?}"
    );
    let other_requests = other.finish();
    assert!(
        other_requests.is_empty(),
        "the unconfigured same-number task is never contacted: {other_requests:?}"
    );

    // A proven task whose stored reference no longer matches the directory
    // proof keeps instead of being read.
    let changed = source.own_task_closure(
        fixture.repo.path(),
        TaskScopeRef {
            task_id: eleven.task_id(),
            provider: "forgejo",
            project: "owner/repo",
            instance: "https://moved.invalid/",
        },
        11,
    );
    assert!(
        changed
            .keep_reason()
            .is_some_and(|reason| reason.contains("changed after the directory proof")),
        "a task reference that moved after the proof keeps: {changed:?}"
    );
}

/// Assert the created issue acquired one lease whose sidecar names the exact
/// durable task, and return that binding.
fn assert_created_task_directory(
    fixture: &Fixture,
    storage: &Storage,
    number: u64,
) -> crate::task_identity::TaskBinding {
    let repo_key = route_repo_key(fixture);
    let bindings =
        crate::task_identity::bindings_for_repo_number(storage, &repo_key, number).unwrap();
    assert_eq!(
        bindings.len(),
        1,
        "create persists exactly one durable task"
    );
    let binding = bindings.into_iter().next().unwrap();
    let identity = fixture.identity();
    let rows = list_all_for_repo(storage, &identity).unwrap();
    let row = rows
        .iter()
        .find(|row| row.has_issue(number))
        .expect("the created issue acquired a directory");
    let task = row
        .tasks
        .first()
        .expect("the created directory names its task");
    let key = binding.key();
    assert_eq!(task.task_id, binding.task_id());
    assert_eq!(task.issue, number);
    assert_eq!(task.provider.as_deref(), Some(key.provider()));
    assert_eq!(task.project.as_deref(), Some(key.project()));
    let expected_external = number.to_string();
    assert_eq!(
        task.external_id.as_deref(),
        Some(expected_external.as_str())
    );
    assert_eq!(task.instance.as_deref(), Some(key.instance()));
    binding
}

#[test]
fn create_acquires_the_exact_task_directory_for_each_provider() {
    {
        let fixture = Fixture::new("create-task-forgejo");
        let storage = fixture.attested_storage();
        let mock = fixture.mock(vec![REPO_JSON.to_owned(), issue_json(7, "open", "[]")]);
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
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        let requests = mock.finish();
        assert_eq!(exit, 0, "Forgejo create must succeed: {requests:?}");
        let binding = assert_created_task_directory(&fixture, &storage, 7);
        assert_eq!(binding.key().provider(), "forgejo");
        assert_eq!(binding.key().instance(), base.trim_end_matches('/'));
        assert_eq!(binding.key().project(), "owner/repo");
    }
    {
        let fixture = Fixture::new("create-task-redmine");
        let storage = fixture.attested_storage();
        let mock = PathMock::start(vec![path_rule(
            "POST",
            "/issues.json",
            &redmine_issue_json(7, 1, "New", &[]),
        )]);
        storage
            .save_credential(Role::Orchestrator, "redmine", "fixture-key")
            .unwrap();
        let base = mock.base.clone();
        let args: Vec<String> = [
            "--output",
            "json",
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "--project-id",
            "p1",
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
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        assert_eq!(exit, 0, "Redmine create must succeed");
        let binding = assert_created_task_directory(&fixture, &storage, 7);
        assert_eq!(binding.key().provider(), "redmine");
        assert_eq!(binding.key().project(), "p1");
    }
    {
        let fixture = Fixture::new("create-task-local");
        let storage = fixture.attested_storage();
        let args: Vec<String> = [
            "--output",
            "json",
            "--provider",
            "local",
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
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        assert_eq!(exit, 0, "Local create must succeed");
        let binding = assert_created_task_directory(&fixture, &storage, 1);
        assert_eq!(binding.key().provider(), "local");
        assert_eq!(binding.key().project(), "default");
    }
}

#[test]
fn routed_close_closure_reads_other_associated_task_not_default() {
    use crate::lifecycle::cleanup::closure::DirectoryIssueClosure;

    let fixture = Fixture::new("close-task-closure");
    let storage = fixture.attested_storage();
    let journals = RedmineJournals::new();
    // Only the other associated number is ever read; the current one is
    // closed by definition and never re-read.
    let mock = PathMock::start(redmine_issue_rules(
        62,
        (5, "Closed".to_owned()),
        (5, "Closed".to_owned()),
        &journals,
    ));
    storage
        .save_global_setting("PHASEGENT_REDMINE_API_BASE", &mock.base)
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-key")
        .unwrap();
    let current = bind_redmine_task(&storage, &fixture, &mock.base, "p1", 61);
    let other = bind_redmine_task(&storage, &fixture, &mock.base, "p1", 62);
    assert_ne!(current.task_id(), other.task_id());
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "local");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let closure = crate::cli::issue_closure::CurrentIssueClosure::new_task_aware(
        fixture.repo.path(),
        Role::Orchestrator,
        crate::cli::provider_context::ProviderContext::default(),
    );
    assert!(
        closure.confirm_others_closed(61, &[61, 62]).is_ok(),
        "the other associated task is read through its own pinned instance"
    );
    let requests = mock.finish();
    assert!(
        requests
            .iter()
            .any(|(_, path, _)| path.starts_with("/issues/62.json")),
        "the other number is read through its durable task, not the default: {requests:?}"
    );

    // A second same-number task makes the other number ambiguous: the closure
    // keeps instead of guessing, and no further default request is made.
    let ambiguous = bind_redmine_task(&storage, &fixture, "https://other.invalid/", "p1", 62);
    assert_ne!(ambiguous.task_id(), other.task_id());
    closure.refresh();
    let error = closure
        .confirm_others_closed(61, &[61, 62])
        .expect_err("an ambiguous other number must keep the directory");
    assert!(
        error.contains("several durable tasks"),
        "bounded ambiguity reason: {error}"
    );
}

#[test]
fn branch_task_reference_precedes_repo_membership() {
    use crate::git_runner::ProcessGitRunner;
    let fixture = Fixture::new("route-branch-precedence");
    let storage = fixture.attested_storage();
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(9, "open", "[]")]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let key = crate::task_identity::TaskKey::new("forgejo", &mock.base, "owner/repo", "9").unwrap();
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).unwrap();
    crate::branch_links::ensure_schema(&storage.connection).unwrap();
    let runner = ProcessGitRunner::in_directory(fixture.repo.path());
    let branch = crate::git_runner::current_branch(&runner).unwrap();
    let repo_key = route_repo_key(&fixture);
    let issue = crate::branch_links::IssueKey::from_number("forgejo", "owner/repo", 9).unwrap();
    crate::branch_links::store::link_many_with_task(
        &storage.connection,
        &[(
            crate::branch_links::LinkParams {
                repo_key: &repo_key,
                branch: &branch,
                issue: &issue,
                issue_number: 9,
                source: "fixture",
                now: 10,
            },
            Some(binding.task_id()),
        )],
    )
    .unwrap();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "local");
    let args: Vec<String> = ["issue", "get", "9"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "branch task reference must route the number");
    assert!(!requests.is_empty());
}

#[test]
fn ambiguous_repo_memberships_fail_before_http() {
    let fixture = Fixture::new("route-ambiguous");
    let storage = fixture.attested_storage();
    bind_task(&storage, &fixture, "http://127.0.0.1:1", 9);
    bind_task(&storage, &fixture, "http://127.0.0.1:2", 9);
    let mock = fixture.zero_mock();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["issue", "get", "9"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "ambiguous memberships must fail closed");
    assert!(
        stderr.contains("multiple durable tasks"),
        "ambiguity diagnostic missing: {stderr}"
    );
    assert!(requests.is_empty());
}

#[test]
fn stored_instance_drift_fails_before_http() {
    let fixture = Fixture::new("route-drift");
    let storage = fixture.attested_storage();
    let binding_instance = "http://127.0.0.1:9";
    bind_task(&storage, &fixture, binding_instance, 9);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            "http://127.0.0.1:10",
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let mock = ZeroMock::start();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["issue", "get", "9"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "endpoint drift must fail closed");
    assert!(
        stderr.contains("does not match the stored task instance"),
        "drift diagnostic missing: {stderr}"
    );
    assert!(requests.is_empty());
}

#[test]
fn forgejo_create_persists_the_task_identity_and_membership() {
    let fixture = Fixture::new("route-create-persist");
    let storage = fixture.attested_storage();
    let mock = fixture.mock(vec![REPO_JSON.to_owned(), issue_json(7, "open", "[]")]);
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
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0);
    let bindings =
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 7)
            .unwrap();
    assert_eq!(bindings.len(), 1, "create must persist exactly one task");
    assert_eq!(bindings[0].key().instance(), base.trim_end_matches('/'));
    assert_eq!(bindings[0].key().project(), "owner/repo");
    assert!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST "))
            .count()
            == 1,
        "create must POST exactly once: {requests:?}"
    );
}

#[test]
fn explicit_forgejo_bind_adopts_and_persists_the_task() {
    let fixture = Fixture::new("route-adopt");
    let storage = fixture.attested_storage();
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(11, "open", "[]")]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let checkout = std::process::Command::new("git")
        .args(["checkout", "-q", "-B", "feat/11"])
        .current_dir(fixture.repo.path())
        .output()
        .unwrap();
    assert!(checkout.status.success());
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--repository",
        "owner/repo",
        "issue",
        "bind",
        "11",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let base = mock.base.clone();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(
        exit, 0,
        "explicit verified adopt must succeed: {requests:?}"
    );
    let bindings =
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 11)
            .unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].key().instance(), base.trim_end_matches('/'));
}

fn seed_local_issue_with_status(fixture: &Fixture, number: u64, status: &str) {
    let path = fixture.db.path().join("local.sqlite3");
    let store = crate::infra::local_store::open_local_at(&path).unwrap();
    store
        .connection
        .execute(
            "INSERT OR IGNORE INTO local_issues (id, title, status, created_at, updated_at) VALUES (?1, 'fixture', ?2, 1, 1)",
            rusqlite::params![number as i64, status],
        )
        .unwrap();
}

fn bind_local_task(fixture: &Fixture, number: u64) -> i64 {
    let storage = Storage::open().unwrap();
    let key = crate::task_identity::TaskKey::new("local", "local", "default", &number.to_string())
        .unwrap();
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).unwrap();
    crate::task_identity::add_repo_membership(
        &storage,
        &route_repo_key(fixture),
        binding.task_id(),
        10,
    )
    .unwrap();
    binding.task_id()
}

#[test]
fn bound_local_task_routes_each_single_number_command_before_the_default() {
    let fixture = Fixture::new("route-local-matrix");
    let counter = RouteCounter::start();
    let storage = fixture.attested_storage();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &counter.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "forgejo");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let cases: &[(&str, &[&str])] = &[
        ("issue-get", &["issue", "get"]),
        ("issue-update", &["issue", "update"]),
        ("issue-close", &["issue", "close"]),
        ("status-next", &["status", "next"]),
        ("status-set", &["status", "set"]),
        ("status-advance", &["status", "advance"]),
    ];
    for (index, (label, prefix)) in cases.iter().enumerate() {
        let number = 900 + index as u64;
        let seed_status = match *label {
            "issue-close" => "Resolved",
            _ => "New",
        };
        seed_local_issue_with_status(&fixture, number, seed_status);
        let task_id = bind_local_task(&fixture, number);
        let number_text = number.to_string();
        let mut args: Vec<String> = prefix.iter().map(|value| (*value).to_owned()).collect();
        args.push(number_text.clone());
        match *label {
            "issue-update" => {
                args.push("--body".to_owned());
                args.push("updated".to_owned());
            }
            "status-set" | "status-advance" => {
                args.push("--status".to_owned());
                args.push("In Progress".to_owned());
            }
            _ => {}
        }
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        assert_eq!(
            exit, 0,
            "{label} must route to the bound Local task: {args:?}"
        );
        let bindings = crate::task_identity::bindings_for_repo_number(
            &storage,
            &route_repo_key(&fixture),
            number,
        )
        .unwrap();
        assert_eq!(bindings.len(), 1, "{label} must not change task identity");
        assert_eq!(bindings[0].task_id(), task_id);
    }
    assert_eq!(
        counter.finish(),
        0,
        "no command may contact the conflicting default endpoint"
    );
}

#[test]
fn create_persistence_failure_warns_without_retrying_the_post() {
    let fixture = Fixture::new("route-create-persist-fail");
    let storage = fixture.attested_storage();
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(7, "open", "[]")]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    storage
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_task_insert BEFORE INSERT ON task_bindings              BEGIN SELECT RAISE(FAIL, 'fixture persistence failure'); END;",
        )
        .unwrap();
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
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "remote success stays exit 0: {stderr}");
    assert!(
        stdout.contains("\"number\":7"),
        "remote summary unchanged: {stdout}"
    );
    assert!(
        stderr.contains("task identity persistence failed") && stderr.contains("issue bind 7"),
        "bounded adoption warning missing: {stderr}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST "))
            .count(),
        1,
        "the POST must never be retried: {requests:?}"
    );
}

#[test]
fn failed_adoption_verification_leaves_no_local_identity() {
    let fixture = Fixture::new("route-adopt-fail");
    let storage = fixture.attested_storage();
    let mock = Mock::start_status(vec![(404, "{}".to_owned())]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let checkout = std::process::Command::new("git")
        .args(["checkout", "-q", "-B", "feat/12"])
        .current_dir(fixture.repo.path())
        .output()
        .unwrap();
    assert!(checkout.status.success());
    let before_bindings =
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 12)
            .unwrap();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--repository",
        "owner/repo",
        "issue",
        "bind",
        "12",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let _requests = mock.finish();
    assert_ne!(exit, 0, "a failed GET must abort adoption");
    let after_bindings =
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 12)
            .unwrap();
    assert_eq!(after_bindings, before_bindings);
    assert!(after_bindings.is_empty());
}

#[test]
fn orphan_branch_task_reference_fails_before_http() {
    use crate::git_runner::ProcessGitRunner;
    let fixture = Fixture::new("route-orphan");
    let storage = fixture.attested_storage();
    crate::branch_links::ensure_schema(&storage.connection).unwrap();
    let runner = ProcessGitRunner::in_directory(fixture.repo.path());
    let branch = crate::git_runner::current_branch(&runner).unwrap();
    let repo_key = route_repo_key(&fixture);
    let issue = crate::branch_links::IssueKey::from_number("forgejo", "owner/repo", 13).unwrap();
    crate::branch_links::store::link_many_with_task(
        &storage.connection,
        &[(
            crate::branch_links::LinkParams {
                repo_key: &repo_key,
                branch: &branch,
                issue: &issue,
                issue_number: 13,
                source: "fixture",
                now: 10,
            },
            Some(9999),
        )],
    )
    .unwrap();
    let mock = fixture.zero_mock();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["issue", "get", "13"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_ne!(exit, 0);
    assert!(stderr.contains("no durable task binding"), "{stderr}");
    assert!(requests.is_empty());
}

#[test]
fn missing_role_credential_fails_before_http_for_a_bound_task() {
    let fixture = Fixture::new("route-missing-credential");
    let storage = fixture.attested_storage();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let binding = bind_task(&storage, &fixture, &mock.base, 14);
    storage
        .connection
        .execute(
            "DELETE FROM role_credential WHERE role = 'orchestrator' AND provider = 'forgejo'",
            [],
        )
        .unwrap();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["issue", "get", "14"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_ne!(exit, 0);
    assert!(stderr.contains("credential is not configured"), "{stderr}");
    assert!(requests.is_empty());
    let bindings =
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 14)
            .unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].task_id(), binding.task_id());
}

fn seed_local_comment(fixture: &Fixture, issue: u64, id: u64, marker: &str) {
    let path = fixture.db.path().join("local.sqlite3");
    let store = crate::infra::local_store::open_local_at(&path).unwrap();
    store
        .connection
        .execute(
            "INSERT OR IGNORE INTO local_comments (id, issue_id, role, phase, attempt, marker, body, created_at) \
             VALUES (?1, ?2, 'executor', '', 1, ?3, 'fixture', 1)",
            rusqlite::params![id as i64, issue as i64, marker],
        )
        .unwrap();
}

#[test]
fn bound_local_task_routes_comment_and_record_commands_before_the_default() {
    let fixture = Fixture::new("route-local-comment-record");
    let counter = RouteCounter::start();
    let storage = fixture.attested_storage();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &counter.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "forgejo");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );

    seed_local_issue_with_status(&fixture, 1001, "New");
    seed_local_comment(&fixture, 1001, 1, "fixture-marker");
    let comment_task = bind_local_task(&fixture, 1001);
    let comment_cases: &[&[&str]] = &[
        &["comment", "get", "1001", "1"],
        &["comment", "list", "1001"],
        &[
            "comment",
            "find-marker",
            "1001",
            "--marker",
            "fixture-marker",
        ],
        &[
            "comment",
            "create",
            "1001",
            "--body",
            "note new-marker",
            "--marker",
            "new-marker",
        ],
    ];
    for case in comment_cases {
        let args: Vec<String> = case.iter().map(|value| (*value).to_owned()).collect();
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        assert_eq!(exit, 0, "comment route failed: {args:?}");
    }
    let bindings =
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 1001)
            .unwrap();
    assert_eq!(bindings[0].task_id(), comment_task);

    seed_local_issue_with_status(&fixture, 1002, "New");
    let record_task = bind_local_task(&fixture, 1002);
    let create_args: Vec<String> = [
        "record",
        "create",
        "1002",
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
    let (exit, stdout, stderr) = run_captured_as(fixture.repo.path(), &create_args, "executor");
    assert_eq!(exit, 0, "record create route failed: {stderr}");
    assert!(stdout.contains("\"id\""), "record create output: {stdout}");
    let record_id = stdout
        .split("\"id\":")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .filter(|value| !value.is_empty())
        .expect("record id");
    let get_args: Vec<String> = vec![
        "record".to_owned(),
        "get".to_owned(),
        "1002".to_owned(),
        record_id.to_owned(),
    ];
    assert_eq!(run_as(fixture.repo.path(), "orchestrator", &get_args), 0);
    let list_args: Vec<String> = ["record", "list", "1002"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(run_as(fixture.repo.path(), "orchestrator", &list_args), 0);
    let bindings =
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 1002)
            .unwrap();
    assert_eq!(bindings[0].task_id(), record_task);
    assert_eq!(
        counter.finish(),
        0,
        "no comment/record route may reach the default"
    );
}

#[test]
fn branch_task_number_mismatch_rejects_the_requested_number() {
    use crate::git_runner::ProcessGitRunner;
    let fixture = Fixture::new("route-branch-mismatch");
    let storage = fixture.attested_storage();
    crate::branch_links::ensure_schema(&storage.connection).unwrap();
    let key =
        crate::task_identity::TaskKey::new("forgejo", "http://127.0.0.1:1", "owner/repo", "8")
            .unwrap();
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).unwrap();
    let runner = ProcessGitRunner::in_directory(fixture.repo.path());
    let branch = crate::git_runner::current_branch(&runner).unwrap();
    let repo_key = route_repo_key(&fixture);
    let issue = crate::branch_links::IssueKey::from_number("forgejo", "owner/repo", 7).unwrap();
    crate::branch_links::store::link_many_with_task(
        &storage.connection,
        &[(
            crate::branch_links::LinkParams {
                repo_key: &repo_key,
                branch: &branch,
                issue: &issue,
                issue_number: 7,
                source: "fixture",
                now: 10,
            },
            Some(binding.task_id()),
        )],
    )
    .unwrap();
    let refs =
        crate::branch_links::reads::branch_task_refs(&storage.connection, &repo_key, &branch)
            .unwrap();
    assert_eq!(refs, vec![(7, binding.task_id())]);
    let mock = fixture.zero_mock();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["issue", "get", "7"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let requests = mock.finish();
    assert_ne!(
        exit, 0,
        "a link row for number 7 must not silently use task 8"
    );
    assert!(
        stderr.contains("does not match") || stderr.contains("durable task"),
        "mismatch diagnostic: {stderr}"
    );
    assert!(requests.is_empty());
}

#[test]
fn explicit_api_base_selects_among_same_number_tasks_without_rewriting() {
    let fixture = Fixture::new("route-explicit-select");
    let storage = fixture.attested_storage();
    let primary = ZeroMock::start();
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(15, "open", "[]")]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    bind_task(&storage, &fixture, &primary.base, 15);
    let selected = bind_task(&storage, &fixture, &mock.base, 15);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &mock.base,
        "issue",
        "get",
        "15",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_eq!(exit, 0, "explicit api-base must select the matching task");
    assert!(
        !requests.is_empty(),
        "the selected instance must serve the get"
    );
    let primary_requests = primary.finish();
    assert!(
        primary_requests.is_empty(),
        "the unselected instance must stay silent"
    );
    let bindings =
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 15)
            .unwrap();
    assert_eq!(bindings.len(), 2, "selection must not rewrite ownership");
    assert!(
        bindings
            .iter()
            .any(|binding| binding.task_id() == selected.task_id())
    );
}

#[test]
fn contradictory_api_base_rejects_before_http() {
    let fixture = Fixture::new("route-explicit-contradiction");
    let storage = fixture.attested_storage();
    let bound = ZeroMock::start();
    let other = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &bound.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    bind_task(&storage, &fixture, &bound.base, 16);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &other.base,
        "issue",
        "get",
        "16",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    assert_ne!(exit, 0, "a contradictory explicit endpoint must reject");
    assert!(
        stderr.contains("does not match durable task") || stderr.contains("does not match"),
        "{stderr}"
    );
    assert!(bound.finish().is_empty());
    assert!(other.finish().is_empty());
}

#[test]
fn bound_local_read_preserves_the_config_store_snapshot() {
    let fixture = Fixture::new("route-readonly-snapshot");
    let storage = fixture.attested_storage();
    seed_local_issue_with_status(&fixture, 1003, "New");
    bind_local_task(&fixture, 1003);
    let before = complete_fixture_snapshot(&storage.connection);
    let args: Vec<String> = ["issue", "get", "1003"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    assert_eq!(exit, 0);
    let after = complete_fixture_snapshot(&storage.connection);
    assert_eq!(
        after, before,
        "a bound read must preserve the whole config store"
    );
}

#[test]
fn same_branch_different_instance_adoption_conflicts_atomically() {
    use crate::git_runner::ProcessGitRunner;
    let fixture = Fixture::new("route-adopt-pk-conflict");
    let storage = fixture.attested_storage();
    crate::branch_links::ensure_schema(&storage.connection).unwrap();
    let checkout = std::process::Command::new("git")
        .args(["checkout", "-q", "-B", "feat/17"])
        .current_dir(fixture.repo.path())
        .output()
        .unwrap();
    assert!(checkout.status.success());
    let first_instance = "http://127.0.0.1:21";
    let first = bind_task(&storage, &fixture, first_instance, 17);
    let runner = ProcessGitRunner::in_directory(fixture.repo.path());
    let branch = crate::git_runner::current_branch(&runner).unwrap();
    let repo_key = route_repo_key(&fixture);
    let issue = crate::branch_links::IssueKey::from_number("forgejo", "owner/repo", 17).unwrap();
    crate::branch_links::store::link_many_with_task(
        &storage.connection,
        &[(
            crate::branch_links::LinkParams {
                repo_key: &repo_key,
                branch: &branch,
                issue: &issue,
                issue_number: 17,
                source: "fixture",
                now: 10,
            },
            Some(first.task_id()),
        )],
    )
    .unwrap();
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(17, "open", "[]")]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--repository",
        "owner/repo",
        "issue",
        "bind",
        "17",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    let _requests = mock.finish();
    assert_ne!(
        exit, 0,
        "a same-branch different instance must fail: {stderr}"
    );
    assert!(
        stderr.contains("another immutable task instance") || stderr.contains("immutable"),
        "{stderr}"
    );
    let bindings = crate::task_identity::bindings_for_repo_number(&storage, &repo_key, 17).unwrap();
    assert_eq!(
        bindings.len(),
        1,
        "no partial second binding may be committed"
    );
    let refs =
        crate::branch_links::reads::branch_task_refs(&storage.connection, &repo_key, &branch)
            .unwrap();
    assert_eq!(
        refs,
        vec![(17, first.task_id())],
        "the branch task ref must be unchanged"
    );
}

fn checkout_numbered_branch(fixture: &Fixture, name: &str) {
    let output = std::process::Command::new("git")
        .args(["checkout", "-q", "-B", name])
        .current_dir(fixture.repo.path())
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn explicit_multi_bind_is_all_or_nothing_before_local_writes() {
    let fixture = Fixture::new("route-multi-bind-atomic");
    let storage = fixture.attested_storage();
    let mock = Mock::start_status(vec![
        (200, REPO_JSON.to_owned()),
        (200, issue_json(21, "open", "[]")),
        (200, REPO_JSON.to_owned()),
        (404, "{}".to_owned()),
    ]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    checkout_numbered_branch(&fixture, "feat/21");
    crate::branch_links::ensure_schema(&storage.connection).unwrap();
    let repo_key = route_repo_key(&fixture);
    let before = complete_fixture_snapshot(&storage.connection);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--repository",
        "owner/repo",
        "issue",
        "bind",
        "21",
        "22",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, _stdout, _stderr) = run_captured(fixture.repo.path(), &args);
    let _requests = mock.finish();
    assert_ne!(
        exit, 0,
        "a failing second identity must abort the whole bind"
    );
    assert!(
        crate::task_identity::bindings_for_repo_number(&storage, &repo_key, 21)
            .unwrap()
            .is_empty()
    );
    assert!(
        crate::task_identity::bindings_for_repo_number(&storage, &repo_key, 22)
            .unwrap()
            .is_empty()
    );
    assert_eq!(complete_fixture_snapshot(&storage.connection), before);
}

#[test]
fn explicit_multi_bind_positive_is_atomic_and_idempotent() {
    let fixture = Fixture::new("route-multi-bind-positive");
    let storage = fixture.attested_storage();
    let mock = Mock::start(vec![
        REPO_JSON.to_owned(),
        issue_json(23, "open", "[]"),
        REPO_JSON.to_owned(),
        issue_json(24, "open", "[]"),
        REPO_JSON.to_owned(),
        issue_json(23, "open", "[]"),
        REPO_JSON.to_owned(),
        issue_json(24, "open", "[]"),
    ]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    checkout_numbered_branch(&fixture, "feat/23");
    let repo_key = route_repo_key(&fixture);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--repository",
        "owner/repo",
        "issue",
        "bind",
        "23",
        "24",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(run_as(fixture.repo.path(), "orchestrator", &args), 0);
    let first = crate::task_identity::bindings_for_repo_number(&storage, &repo_key, 23).unwrap();
    let second = crate::task_identity::bindings_for_repo_number(&storage, &repo_key, 24).unwrap();
    assert_eq!((first.len(), second.len()), (1, 1));
    let refs =
        crate::branch_links::reads::branch_task_refs(&storage.connection, &repo_key, "feat/23")
            .unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(run_as(fixture.repo.path(), "orchestrator", &args), 0);
    assert_eq!(
        crate::task_identity::bindings_for_repo_number(&storage, &repo_key, 23)
            .unwrap()
            .len(),
        1,
        "repeat bind must not duplicate task identity"
    );
}

fn spawn_fixed_json_responder(body: &'static str) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        while !data.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read]);
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    (base, task)
}

#[test]
fn redmine_adoption_rejects_a_wrong_project() {
    let fixture = Fixture::new("route-adopt-wrong-project");
    let storage = fixture.attested_storage();
    let body = r#"{"issue":{"id":25,"subject":"fixture","description":"","project":{"id":2,"name":"other","identifier":"other"},"status":{"name":"New","is_closed":false},"journals":[]}}"#;
    let (base, responder) = spawn_fixed_json_responder(body);
    storage
        .save_global_setting("PHASEGENT_REDMINE_API_BASE", &base)
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-key")
        .unwrap();
    checkout_numbered_branch(&fixture, "feat/25");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "--provider",
        "redmine",
        "--project-id",
        "p1",
        "issue",
        "bind",
        "25",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    responder.join().unwrap();
    assert_ne!(
        exit, 0,
        "a wrong-project issue must not be adopted: {stderr}"
    );
    assert!(
        stderr.contains("different Redmine project"),
        "wrong-project diagnostic: {stderr}"
    );
    assert!(
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 25)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn explicit_api_base_cannot_attest_credential_provenance_for_bind() {
    let fixture = Fixture::new("route-bind-provenance");
    let storage = fixture.attested_storage();
    let stored = ZeroMock::start();
    let explicit = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &stored.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    checkout_numbered_branch(&fixture, "feat/26");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        &explicit.base,
        "--repository",
        "owner/repo",
        "issue",
        "bind",
        "26",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, _stdout, stderr) = run_captured(fixture.repo.path(), &args);
    assert_ne!(
        exit, 0,
        "explicit endpoint must not attest provenance: {stderr}"
    );
    assert!(stored.finish().is_empty());
    assert!(explicit.finish().is_empty());
    assert!(
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 26)
            .unwrap()
            .is_empty()
    );
}

fn spawn_capturing_responder(body: &'static str) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        while !data.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read]);
        }
        let request = String::from_utf8_lossy(&data).into_owned();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
        request
    });
    (base, task)
}

#[test]
fn bound_redmine_task_routes_get_against_the_stored_instance() {
    let fixture = Fixture::new("route-redmine-get");
    let storage = fixture.attested_storage();
    let body = r#"{"issue":{"id":27,"subject":"fixture","description":"","project":{"id":1,"name":"p1","identifier":"p1"},"status":{"name":"New","is_closed":false},"journals":[]}}"#;
    let (base, responder) = spawn_capturing_responder(body);
    storage
        .save_global_setting("PHASEGENT_REDMINE_API_BASE", &base)
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-key")
        .unwrap();
    let key = crate::task_identity::TaskKey::new("redmine", &base, "p1", "27").unwrap();
    let binding = crate::task_identity::ensure_binding(&storage, &key, 10).unwrap();
    crate::task_identity::add_repo_membership(
        &storage,
        &route_repo_key(&fixture),
        binding.task_id(),
        10,
    )
    .unwrap();
    let counter = RouteCounter::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &counter.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "forgejo");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["issue", "get", "27"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let request = responder.join().unwrap();
    assert_eq!(exit, 0, "the bound Redmine task must serve the get");
    assert!(
        request.starts_with("GET ") && request.contains("/issues/27.json"),
        "unexpected Redmine request: {request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-redmine-api-key: fixture-key"),
        "the fixture credential must authenticate the pinned instance: {request}"
    );
    assert_eq!(counter.finish(), 0, "the Forgejo default must stay silent");
}

#[test]
fn create_without_a_repository_persists_the_key_without_membership() {
    let fixture = Fixture::new("route-create-nonrepo");
    let storage = fixture.attested_storage();
    let mock = Mock::start(vec![REPO_JSON.to_owned(), issue_json(28, "open", "[]")]);
    let base = mock.base.clone();
    storage
        .save_forgejo_auth(Role::Orchestrator, &base, "owner/repo", "fixture-token")
        .unwrap();
    let nonrepo = crate::test_scratch::root().join(format!(
        "phasegent-nonrepo-{}-{}",
        std::process::id(),
        crate::worktree::now_unix_secs()
    ));
    std::fs::create_dir_all(&nonrepo).unwrap();
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
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let (exit, stdout, stderr) = run_captured(&nonrepo, &args);
    let requests = mock.finish();
    let _ = std::fs::remove_dir_all(&nonrepo);
    assert_eq!(exit, 0, "nonrepo create stays successful: {stderr}");
    assert!(stdout.contains("\"number\":28"));
    assert!(
        stderr.contains("without repository association"),
        "missing repo warning: {stderr}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST "))
            .count(),
        1
    );
    let key = crate::task_identity::TaskKey::new("forgejo", &base, "owner/repo", "28").unwrap();
    let binding =
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 28)
            .unwrap();
    assert!(binding.is_empty(), "no membership may be invented");
    let stored = storage
        .connection
        .query_row(
            "SELECT task_id FROM task_bindings WHERE provider='forgejo' AND instance=?1 AND project='owner/repo' AND external_id='28'",
            [base.trim_end_matches('/')],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert!(stored > 0);
    let _ = key;
}

fn spawn_sequence_responder<B: Into<String> + Send + 'static>(
    bodies: Vec<B>,
) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let base_for_bodies = base.clone();
    let task = thread::spawn(move || {
        let mut requests = Vec::new();
        for body in bodies {
            let body = body.into().replace("{base}", &base_for_bodies);
            let (mut stream, _) = listener.accept().unwrap();
            let mut data = Vec::new();
            let mut buffer = [0u8; 4096];
            while !data.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                data.extend_from_slice(&buffer[..read]);
            }
            requests.push(String::from_utf8_lossy(&data).into_owned());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    (base, task)
}

#[test]
fn bound_forgejo_task_routes_update_and_comment_methods() {
    let fixture = Fixture::new("route-forgejo-methods");
    let storage = fixture.attested_storage();

    let (base, responder) = spawn_sequence_responder(vec![
        REPO_JSON.to_owned(),
        r#"{"id":531,"number":31,"title":"fixture","body":"before","state":"open","labels":[]}"#
            .to_owned(),
        r#"{"id":531,"number":31,"title":"fixture","body":"after","state":"open","labels":[]}"#
            .to_owned(),
    ]);
    storage
        .save_forgejo_auth(Role::Orchestrator, &base, "owner/repo", "fixture-token")
        .unwrap();
    bind_task(&storage, &fixture, &base, 31);
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let args: Vec<String> = ["issue", "update", "31", "--body", "after"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(run_as(fixture.repo.path(), "orchestrator", &args), 0);
    let requests = responder.join().unwrap();
    assert!(
        requests.iter().any(|request| request.starts_with("PATCH ")
            && request.contains("/repos/owner/repo/issues/31")),
        "update must PATCH the pinned issue path: {requests:?}"
    );

    let cases: Vec<(&str, u64, Vec<String>)> = vec![
        (
            "create",
            32,
            vec![
                "comment".into(),
                "create".into(),
                "32".into(),
                "--body".into(),
                "note fixture-marker".into(),
                "--marker".into(),
                "fixture-marker".into(),
            ],
        ),
        (
            "list",
            33,
            vec!["comment".into(), "list".into(), "33".into()],
        ),
        (
            "get",
            34,
            vec!["comment".into(), "get".into(), "34".into(), "1".into()],
        ),
        (
            "find",
            35,
            vec![
                "comment".into(),
                "find-marker".into(),
                "35".into(),
                "--marker".into(),
                "fixture-marker".into(),
            ],
        ),
    ];
    for (label, number, args) in cases {
        let reply = if label == "create" {
            format!(
                r#"{{"id":1,"body":"note fixture-marker","issue_url":"{{base}}/api/v1/repos/owner/repo/issues/{number}","html_url":null}}"#
            )
        } else {
            format!(
                r#"[{{"id":1,"body":"note fixture-marker","issue_url":"{{base}}/api/v1/repos/owner/repo/issues/{number}","html_url":null}}]"#
            )
        };
        let mut bodies = vec![
            REPO_JSON.to_owned(),
            issue_json(number, "open", "[]"),
            reply,
        ];
        if label != "create" {
            bodies.push("[]".to_owned());
        }
        let (base, responder) = spawn_sequence_responder(bodies);
        storage
            .save_forgejo_auth(Role::Orchestrator, &base, "owner/repo", "fixture-token")
            .unwrap();
        bind_task(&storage, &fixture, &base, number);
        let exit = run_as(fixture.repo.path(), "orchestrator", &args);
        let requests = responder.join().unwrap();
        assert_eq!(exit, 0, "comment {label} must route: {requests:?}");
        if label == "create" {
            assert!(
                requests.iter().any(|request| request.starts_with("POST ")
                    && request.contains("/issues/32/comments")),
                "comment create POST path: {requests:?}"
            );
        } else {
            assert!(
                requests.iter().any(|request| request.starts_with("GET ")
                    && request.contains(&format!("/issues/{number}/comments"))),
                "comment {label} GET path: {requests:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Issue 1201 P6 J3: the per-route matrix for both remote provider classes.
//
// The fixtures below answer by method and path instead of by call order, so
// one listener can serve every command of a matrix row and the recorded
// traffic is the exact traffic of the invocation.
// ---------------------------------------------------------------------------

/// A loopback HTTP responder that dispatches on request method and path.
///
/// The listener keeps accepting until the command under test has finished,
/// so the recorded requests cover the whole lifetime of that invocation
/// rather than a sample taken over a timer window.
struct PathMock {
    base: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    task: Option<JoinHandle<()>>,
}

/// A rule body. It receives the request target (path plus query), the
/// request body and the listener's base URL, and answers with a status code
/// and a response body.
type PathResponder = std::sync::Arc<dyn Fn(&str, &str, &str) -> (u16, String) + Send + Sync>;

struct PathRule {
    method: String,
    path: String,
    responder: PathResponder,
}

fn path_rule(method: &str, path: &str, body: &str) -> PathRule {
    let body = body.to_owned();
    PathRule {
        method: method.to_owned(),
        path: path.to_owned(),
        responder: std::sync::Arc::new(move |_, _, _| (200, body.clone())),
    }
}

fn path_rule_fn(
    method: &str,
    path: &str,
    responder: impl Fn(&str, &str, &str) -> (u16, String) + Send + Sync + 'static,
) -> PathRule {
    PathRule {
        method: method.to_owned(),
        path: path.to_owned(),
        responder: std::sync::Arc::new(responder),
    }
}

fn read_http_request(stream: &mut impl Read) -> String {
    let mut data = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let size = match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => size,
            Err(_) => break,
        };
        data.extend_from_slice(&buffer[..size]);
        let Some(end) = data.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
        let length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length: ")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if data.len() >= end + 4 + length {
            break;
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

fn parse_http_request(raw: &str) -> (String, String, String) {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let request_line = head.split("\r\n").next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let target = parts.next().unwrap_or_default().to_owned();
    (method, target, body.to_owned())
}

impl PathMock {
    fn start(rules: Vec<PathRule>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let log = std::sync::Arc::clone(&requests);
        let done = std::sync::Arc::clone(&stop);
        let listener_base = base.clone();
        let task = thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                            .unwrap();
                        let raw = read_http_request(&mut stream);
                        let (method, target, body) = parse_http_request(&raw);
                        log.lock()
                            .unwrap()
                            .push((method.clone(), target.clone(), body.clone()));
                        let path = target.split('?').next().unwrap_or_default().to_owned();
                        let (status, response_body) = rules
                            .iter()
                            .find(|rule| rule.method == method && rule.path == path)
                            .map(|rule| (rule.responder)(&target, &body, &listener_base))
                            .unwrap_or_else(|| {
                                (404, r#"{"errors":["no fixture route"]}"#.to_owned())
                            });
                        let reason = if status == 200 { "OK" } else { "Error" };
                        let response = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                            response_body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if done.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base,
            requests,
            stop,
            task: Some(task),
        }
    }

    fn finish(mut self) -> Vec<(String, String, String)> {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for PathMock {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

/// Whether the recorded traffic contains one request with this method and
/// path (the query string is not part of the comparison).
fn requests_hit(requests: &[(String, String, String)], method: &str, path: &str) -> bool {
    requests.iter().any(|(seen_method, target, _)| {
        seen_method == method && target.split('?').next().unwrap_or_default() == path
    })
}

/// A JSON string field addressed by a path of object keys.
fn json_string_field(payload: &str, path: &[&str]) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return String::new();
    };
    let mut current = &value;
    for key in path {
        match current.get(*key) {
            Some(next) => current = next,
            None => return String::new(),
        }
    }
    current.as_str().unwrap_or_default().to_owned()
}

fn forgejo_comment_json(id: u64, body: &str, issue_url: &str) -> String {
    serde_json::json!({
        "id": id,
        "body": body,
        "issue_url": issue_url,
        "html_url": null,
    })
    .to_string()
}

const REDMINE_STATUSES_JSON: &str = r#"{"issue_statuses":[{"id":1,"name":"New"},{"id":2,"name":"In Progress"},{"id":3,"name":"In Review"},{"id":4,"name":"Resolved"},{"id":5,"name":"Closed"}]}"#;

fn redmine_issue_json(number: u64, status_id: u64, status_name: &str, notes: &[String]) -> String {
    let journals: Vec<serde_json::Value> = notes
        .iter()
        .enumerate()
        .map(|(index, note)| serde_json::json!({"id": index as u64 + 1, "notes": note}))
        .collect();
    serde_json::json!({
        "issue": {
            "id": number,
            "subject": "fixture",
            "description": "",
            "project": {"id": 1, "name": "p1", "identifier": "p1"},
            "status": {
                "id": status_id,
                "name": status_name,
                "is_closed": status_name == "Closed",
            },
            "journals": journals,
        }
    })
    .to_string()
}

/// The journals one Redmine fixture issue carries, in write order.
#[derive(Clone)]
struct RedmineJournals(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl RedmineJournals {
    fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())))
    }

    fn push(&self, notes: String) {
        self.0.lock().unwrap().push(notes);
    }

    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// The read and write rules for one Redmine issue number. A write that
/// carries notes appends a journal, so the confirmation read sees exactly
/// what the write produced.
fn redmine_issue_rules(
    number: u64,
    get_status: (u64, String),
    put_status: (u64, String),
    journals: &RedmineJournals,
) -> Vec<PathRule> {
    let path = format!("/issues/{number}.json");
    let read_journals = journals.clone();
    let write_journals = journals.clone();
    let read_status = get_status;
    let write_status = put_status;
    vec![
        path_rule_fn("GET", &path, move |_, _, _| {
            (
                200,
                redmine_issue_json(number, read_status.0, &read_status.1, &read_journals.all()),
            )
        }),
        path_rule_fn("PUT", &path, move |_, body, _| {
            let notes = json_string_field(body, &["issue", "notes"]);
            if !notes.is_empty() {
                write_journals.push(notes);
            }
            (
                200,
                redmine_issue_json(
                    number,
                    write_status.0,
                    &write_status.1,
                    &write_journals.all(),
                ),
            )
        }),
    ]
}

fn bind_redmine_task(
    storage: &Storage,
    fixture: &Fixture,
    instance: &str,
    project: &str,
    number: u64,
) -> crate::task_identity::TaskBinding {
    let key = crate::task_identity::TaskKey::new("redmine", instance, project, &number.to_string())
        .unwrap();
    let binding = crate::task_identity::ensure_binding(storage, &key, 10).unwrap();
    crate::task_identity::add_repo_membership(
        storage,
        &route_repo_key(fixture),
        binding.task_id(),
        10,
    )
    .unwrap();
    binding
}

fn args_of(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

#[test]
fn bound_forgejo_task_routes_each_single_number_command_before_the_default() {
    let fixture = Fixture::new("j3-forgejo-matrix");
    let storage = fixture.attested_storage();
    // The conflicting default is Redmine. Any request that reaches it
    // proves the durable route was bypassed.
    let counter = RouteCounter::start();
    let written_comment = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let written_record = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let mut rules = vec![path_rule("GET", "/api/v1/repos/owner/repo", REPO_JSON)];
    // The read and update rows.
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/41",
        &issue_json(41, "open", "[]"),
    ));
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/42",
        &issue_json(42, "open", "[]"),
    ));
    rules.push(path_rule(
        "PATCH",
        "/api/v1/repos/owner/repo/issues/42",
        &issue_json(42, "open", "[]"),
    ));
    // A closeable issue: the native open state moves to the native closed
    // state after the dependency preflight.
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/43",
        &issue_json(43, "open", "[]"),
    ));
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/43/dependencies",
        "[]",
    ));
    rules.push(path_rule(
        "PATCH",
        "/api/v1/repos/owner/repo/issues/43",
        &issue_json(43, "closed", "[]"),
    ));
    // The status rows: the native state is read, never a label catalogue.
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/44",
        &issue_json(44, "open", "[]"),
    ));
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/45",
        &issue_json(45, "open", "[]"),
    ));
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/45/dependencies",
        "[]",
    ));
    rules.push(path_rule(
        "PATCH",
        "/api/v1/repos/owner/repo/issues/45",
        &issue_json(45, "closed", "[]"),
    ));
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/46",
        &issue_json(46, "closed", "[]"),
    ));
    rules.push(path_rule(
        "PATCH",
        "/api/v1/repos/owner/repo/issues/46",
        &issue_json(46, "open", "[]"),
    ));
    // The comment row: the written comment is echoed back by the fixture.
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/47",
        &issue_json(47, "open", "[]"),
    ));
    let read_comment = std::sync::Arc::clone(&written_comment);
    rules.push(path_rule_fn(
        "GET",
        "/api/v1/repos/owner/repo/issues/47/comments",
        move |target, _, base| {
            if target.contains("page=2") {
                return (200, "[]".to_owned());
            }
            match read_comment.lock().unwrap().clone() {
                Some(body) => (
                    200,
                    format!(
                        "[{}]",
                        forgejo_comment_json(
                            1,
                            &body,
                            &format!("{base}/api/v1/repos/owner/repo/issues/47")
                        )
                    ),
                ),
                None => (200, "[]".to_owned()),
            }
        },
    ));
    let write_comment = std::sync::Arc::clone(&written_comment);
    rules.push(path_rule_fn(
        "POST",
        "/api/v1/repos/owner/repo/issues/47/comments",
        move |_, body, base| {
            let text = json_string_field(body, &["body"]);
            *write_comment.lock().unwrap() = Some(text.clone());
            (
                200,
                forgejo_comment_json(
                    1,
                    &text,
                    &format!("{base}/api/v1/repos/owner/repo/issues/47"),
                ),
            )
        },
    ));
    // The record row.
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/51",
        &issue_json(51, "open", "[]"),
    ));
    let read_record = std::sync::Arc::clone(&written_record);
    rules.push(path_rule_fn(
        "GET",
        "/api/v1/repos/owner/repo/issues/51/comments",
        move |target, _, base| {
            if target.contains("page=2") {
                return (200, "[]".to_owned());
            }
            match read_record.lock().unwrap().clone() {
                Some(body) => (
                    200,
                    format!(
                        "[{}]",
                        forgejo_comment_json(
                            1,
                            &body,
                            &format!("{base}/api/v1/repos/owner/repo/issues/51")
                        )
                    ),
                ),
                None => (200, "[]".to_owned()),
            }
        },
    ));
    let write_record = std::sync::Arc::clone(&written_record);
    rules.push(path_rule_fn(
        "POST",
        "/api/v1/repos/owner/repo/issues/51/comments",
        move |_, body, base| {
            let text = json_string_field(body, &["body"]);
            *write_record.lock().unwrap() = Some(text.clone());
            (
                200,
                forgejo_comment_json(1, &text, &format!("{base}/owner/repo/issues/51")),
            )
        },
    ));
    rules.push(path_rule(
        "GET",
        "/api/v1/repos/owner/repo/issues/52",
        &issue_json(52, "open", "[]"),
    ));
    rules.push(path_rule_fn(
        "GET",
        "/api/v1/repos/owner/repo/issues/52/comments",
        |_, _, base| {
            (
                200,
                format!(
                    "[{}]",
                    forgejo_comment_json(
                        9,
                        "repeat",
                        &format!("{base}/api/v1/repos/owner/repo/issues/52")
                    )
                ),
            )
        },
    ));
    let mock = PathMock::start(rules);

    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    storage
        .save_forgejo_auth(Role::Executor, &mock.base, "owner/repo", "fixture-token")
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-key")
        .unwrap();
    let mut identities = Vec::new();
    for number in [41u64, 42, 43, 44, 45, 46, 47, 51, 52] {
        identities.push((number, bind_task(&storage, &fixture, &mock.base, number)));
    }
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "redmine");
    let _redmine_base = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", &counter.base);
    let _close_status = EnvGuard::set("PHASEGENT_CLOSE_STATUS_ID", "5");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );

    let cases: Vec<(&str, Vec<String>, &str, &str)> = vec![
        (
            "issue-get",
            args_of(&["issue", "get", "41"]),
            "GET",
            "/api/v1/repos/owner/repo/issues/41",
        ),
        (
            "issue-update",
            args_of(&["issue", "update", "42", "--body", "after"]),
            "PATCH",
            "/api/v1/repos/owner/repo/issues/42",
        ),
        (
            "issue-close",
            args_of(&["issue", "close", "43"]),
            "PATCH",
            "/api/v1/repos/owner/repo/issues/43",
        ),
        (
            "status-next",
            args_of(&["status", "next", "44"]),
            "GET",
            "/api/v1/repos/owner/repo/issues/44",
        ),
        (
            "status-set",
            args_of(&["status", "set", "45", "--status", "Closed"]),
            "PATCH",
            "/api/v1/repos/owner/repo/issues/45",
        ),
        (
            "status-advance",
            args_of(&["status", "advance", "46", "--status", "Open"]),
            "PATCH",
            "/api/v1/repos/owner/repo/issues/46",
        ),
        (
            "comment-create",
            args_of(&[
                "comment",
                "create",
                "47",
                "--body",
                "note j3-forgejo-comment",
                "--marker",
                "j3-forgejo-comment",
            ]),
            "POST",
            "/api/v1/repos/owner/repo/issues/47/comments",
        ),
        (
            "comment-get",
            args_of(&["comment", "get", "47", "1"]),
            "GET",
            "/api/v1/repos/owner/repo/issues/47/comments",
        ),
        (
            "comment-list",
            args_of(&["comment", "list", "47"]),
            "GET",
            "/api/v1/repos/owner/repo/issues/47/comments",
        ),
        (
            "comment-find-marker",
            args_of(&[
                "comment",
                "find-marker",
                "47",
                "--marker",
                "j3-forgejo-comment",
            ]),
            "GET",
            "/api/v1/repos/owner/repo/issues/47/comments",
        ),
    ];
    for (label, args, _, _) in &cases {
        let exit = run_as(fixture.repo.path(), "orchestrator", args);
        assert_eq!(exit, 0, "{label} must route to the bound Forgejo task");
    }

    // The record wrapper is exercised through its own commands, not only
    // through the comment primitive.
    let create_args = args_of(&[
        "record",
        "create",
        "51",
        "--key",
        "j3-forgejo-record",
        "--phase",
        "P6",
        "--attempt",
        "9",
        "--body",
        "note",
        "--authorized",
    ]);
    assert_eq!(
        run_as(fixture.repo.path(), "executor", &create_args),
        0,
        "record create must route to the bound Forgejo task"
    );
    assert_eq!(
        run_as(fixture.repo.path(), "executor", &create_args),
        0,
        "an identical retry must return the existing record"
    );
    let repeated = args_of(&[
        "record",
        "create",
        "52",
        "--key",
        "j3-forgejo-repeat",
        "--phase",
        "P6",
        "--attempt",
        "9",
        "--body",
        "note",
        "--authorized",
    ]);
    assert_ne!(
        run_as(fixture.repo.path(), "executor", &repeated),
        0,
        "a repeated pagination page must fail the pre-read before writing"
    );
    assert_eq!(
        run_as(
            fixture.repo.path(),
            "orchestrator",
            &args_of(&["record", "get", "51", "1"])
        ),
        0,
        "record get must read the record from the bound Forgejo task"
    );
    assert_eq!(
        run_as(
            fixture.repo.path(),
            "orchestrator",
            &args_of(&["record", "list", "51"])
        ),
        0,
        "record list must list the records of the bound Forgejo task"
    );

    let requests = mock.finish();
    for (label, _, method, path) in &cases {
        assert!(
            requests_hit(&requests, method, path),
            "{label} must reach the pinned instance with {method} {path}: {requests:?}"
        );
    }
    assert!(
        requests_hit(
            &requests,
            "POST",
            "/api/v1/repos/owner/repo/issues/51/comments"
        ),
        "record create must POST the comment on the pinned instance: {requests:?}"
    );
    assert!(
        requests_hit(
            &requests,
            "GET",
            "/api/v1/repos/owner/repo/issues/51/comments"
        ),
        "record read must GET the comments on the pinned instance: {requests:?}"
    );
    let record_write = requests
        .iter()
        .find(|(method, target, _)| {
            method == "POST" && target == "/api/v1/repos/owner/repo/issues/51/comments"
        })
        .map(|(_, _, body)| body.clone())
        .expect("the record write must be recorded");
    assert!(
        record_write.contains("phasegent-record") && record_write.contains("j3-forgejo-record"),
        "the record write must carry the CLI-owned envelope: {record_write}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(method, target, _)| {
                method == "POST" && target == "/api/v1/repos/owner/repo/issues/51/comments"
            })
            .count(),
        1,
        "an identical record retry must not POST again: {requests:?}"
    );
    assert!(
        !requests_hit(
            &requests,
            "POST",
            "/api/v1/repos/owner/repo/issues/52/comments"
        ),
        "a repeated pre-read page must fail before any write: {requests:?}"
    );
    assert_eq!(
        counter.finish(),
        0,
        "no command may contact the conflicting default endpoint"
    );
    for (number, binding) in &identities {
        let bindings = crate::task_identity::bindings_for_repo_number(
            &storage,
            &route_repo_key(&fixture),
            *number,
        )
        .unwrap();
        assert_eq!(bindings.len(), 1, "task {number} must keep one identity");
        assert_eq!(bindings[0].task_id(), binding.task_id());
    }
}

#[test]
fn bound_redmine_task_routes_each_single_number_command_before_the_default() {
    let fixture = Fixture::new("j3-redmine-matrix");
    let storage = fixture.attested_storage();
    // The conflicting default is Forgejo.
    let counter = RouteCounter::start();
    let mut rules = vec![
        path_rule("GET", "/issue_statuses.json", REDMINE_STATUSES_JSON),
        path_rule("GET", "/issues.json", r#"{"issues":[],"total_count":0}"#),
    ];
    let mut identities = Vec::new();
    let mut numbers = Vec::new();
    for number in [51u64, 52, 53, 54, 55, 56, 57, 58] {
        numbers.push(number);
        let journals = RedmineJournals::new();
        let (get_status, put_status) = match number {
            // The close row answers with the configured close status.
            52 => ((1, "New".to_owned()), (5, "Closed".to_owned())),
            // The status rows answer with the target status on the write.
            56 | 57 => ((1, "New".to_owned()), (2, "In Progress".to_owned())),
            _ => ((1, "New".to_owned()), (1, "New".to_owned())),
        };
        rules.extend(redmine_issue_rules(
            number, get_status, put_status, &journals,
        ));
    }
    let mock = PathMock::start(rules);

    storage
        .save_global_setting("PHASEGENT_REDMINE_API_BASE", &mock.base)
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-key")
        .unwrap();
    storage
        .save_credential(Role::Executor, "redmine", "fixture-key")
        .unwrap();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &counter.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    for number in &numbers {
        identities.push((
            *number,
            bind_redmine_task(&storage, &fixture, &mock.base, "p1", *number),
        ));
    }
    let _unset = UnsetEnv::apply(&["PHASEGENT_REDMINE_API_BASE"]);
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "forgejo");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );

    let cases: Vec<(&str, Vec<String>, &str, &str)> = vec![
        (
            "issue-get",
            args_of(&["issue", "get", "58"]),
            "GET",
            "/issues/58.json",
        ),
        (
            "issue-update",
            args_of(&["issue", "update", "51", "--body", "after"]),
            "PUT",
            "/issues/51.json",
        ),
        (
            "issue-close",
            args_of(&["--close-status-id", "5", "issue", "close", "52"]),
            "PUT",
            "/issues/52.json",
        ),
        (
            "comment-create",
            args_of(&[
                "comment",
                "create",
                "53",
                "--body",
                "note j3-redmine-comment",
                "--marker",
                "j3-redmine-comment",
            ]),
            "PUT",
            "/issues/53.json",
        ),
        (
            "comment-get",
            args_of(&["comment", "get", "53", "1"]),
            "GET",
            "/issues/53.json",
        ),
        (
            "comment-list",
            args_of(&["comment", "list", "53"]),
            "GET",
            "/issues/53.json",
        ),
        (
            "comment-find-marker",
            args_of(&[
                "comment",
                "find-marker",
                "53",
                "--marker",
                "j3-redmine-comment",
            ]),
            "GET",
            "/issues/53.json",
        ),
        (
            "status-next",
            args_of(&["status", "next", "55"]),
            "GET",
            "/issues/55.json",
        ),
        (
            "status-set",
            args_of(&["status", "set", "56", "--status", "In Progress"]),
            "PUT",
            "/issues/56.json",
        ),
        (
            "status-advance",
            args_of(&["status", "advance", "57", "--status", "In Progress"]),
            "PUT",
            "/issues/57.json",
        ),
    ];
    for (label, args, _, _) in &cases {
        let exit = run_as(fixture.repo.path(), "orchestrator", args);
        assert_eq!(exit, 0, "{label} must route to the bound Redmine task");
    }

    let create_args = args_of(&[
        "record",
        "create",
        "54",
        "--key",
        "j3-redmine-record",
        "--phase",
        "P6",
        "--attempt",
        "9",
        "--body",
        "note",
        "--authorized",
    ]);
    assert_eq!(
        run_as(fixture.repo.path(), "executor", &create_args),
        0,
        "record create must route to the bound Redmine task"
    );
    assert_eq!(
        run_as(
            fixture.repo.path(),
            "orchestrator",
            &args_of(&["record", "get", "54", "1"])
        ),
        0,
        "record get must read the record from the bound Redmine task"
    );
    assert_eq!(
        run_as(
            fixture.repo.path(),
            "orchestrator",
            &args_of(&["record", "list", "54"])
        ),
        0,
        "record list must list the records of the bound Redmine task"
    );

    let requests = mock.finish();
    for (label, _, method, path) in &cases {
        assert!(
            requests_hit(&requests, method, path),
            "{label} must reach the pinned instance with {method} {path}: {requests:?}"
        );
    }
    assert!(
        requests_hit(&requests, "PUT", "/issues/54.json"),
        "record create must write the journal on the pinned instance: {requests:?}"
    );
    assert!(
        requests_hit(&requests, "GET", "/issues/54.json"),
        "record read must read the issue on the pinned instance: {requests:?}"
    );
    let record_write = requests
        .iter()
        .find(|(method, target, _)| method == "PUT" && target == "/issues/54.json")
        .map(|(_, _, body)| body.clone())
        .expect("the record write must be recorded");
    assert!(
        record_write.contains("phasegent-record") && record_write.contains("j3-redmine-record"),
        "the record write must carry the CLI-owned envelope: {record_write}"
    );
    assert_eq!(
        counter.finish(),
        0,
        "no command may contact the conflicting default endpoint"
    );
    for (number, binding) in &identities {
        let bindings = crate::task_identity::bindings_for_repo_number(
            &storage,
            &route_repo_key(&fixture),
            *number,
        )
        .unwrap();
        assert_eq!(bindings.len(), 1, "task {number} must keep one identity");
        assert_eq!(bindings[0].task_id(), binding.task_id());
    }
}

#[test]
fn explicit_forgejo_bind_rejects_a_pull_request_document() {
    let fixture = Fixture::new("j3-forgejo-pr");
    let storage = fixture.attested_storage();
    let mock = PathMock::start(vec![
        path_rule("GET", "/api/v1/repos/owner/repo", REPO_JSON),
        path_rule(
            "GET",
            "/api/v1/repos/owner/repo/issues/61",
            r#"{"id":561,"number":61,"title":"fixture","body":"fixture","state":"open","labels":[],"pull_request":{"merged":false}}"#,
        ),
    ]);
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    checkout_numbered_branch(&fixture, "feat/61");
    let args = args_of(&[
        "--provider",
        "forgejo",
        "--repository",
        "owner/repo",
        "issue",
        "bind",
        "61",
    ]);
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "a pull request is not an adoptable issue");
    assert!(
        crate::task_identity::bindings_for_repo_number(&storage, &route_repo_key(&fixture), 61)
            .unwrap()
            .is_empty(),
        "a pull request must leave no local identity"
    );
    assert!(
        requests_hit(&requests, "GET", "/api/v1/repos/owner/repo/issues/61"),
        "the fixture must have served the pull request document: {requests:?}"
    );
}

#[test]
fn unbound_number_with_another_scope_link_refuses_before_http_under_a_forgejo_default() {
    let fixture = Fixture::new("default-other-scope");
    let storage = fixture.attested_storage();
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
        .unwrap();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    // The branch link names another repository scope, so the resolved default
    // must refuse before any provider request.
    fixture.link(7, "other/repo");
    let args: Vec<String> = ["issue", "get", "7"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(exit, 0, "another scope refuses the default");
    assert!(
        requests.is_empty(),
        "the refusal happens before any HTTP: {requests:?}"
    );
}

#[test]
fn conflicting_scope_refusal_preserves_legacy_configuration_rows() {
    let fixture = Fixture::new("conflicting-scope-read-only-review");
    let storage = fixture.attested_storage();
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
        .unwrap();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    fixture.link(7, "other/repo");
    storage
        .connection
        .execute(
            "INSERT OR REPLACE INTO role_redmine_config (role, project_id) VALUES ('reviewer', 'legacy-project')",
            [],
        )
        .unwrap();
    let before = complete_fixture_snapshot(&storage.connection);
    let args: Vec<String> = ["issue", "get", "7"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &args);
    let requests = mock.finish();
    assert_ne!(exit, 0);
    assert!(requests.is_empty());
    assert_eq!(
        complete_fixture_snapshot(&storage.connection),
        before,
        "refusal must not mutate configuration or schema"
    );
}

#[test]
fn explicit_redmine_and_local_override_a_forgejo_default() {
    let fixture = Fixture::new("override-default");
    let storage = fixture.attested_storage();
    let _env = UnsetEnv::apply(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
        .unwrap();
    let mock = ZeroMock::start();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            &mock.base,
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let local: Vec<String> = ["--provider", "local", "status", "list"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let exit = run_as(fixture.repo.path(), "orchestrator", &local);
    assert_eq!(exit, 0, "explicit local overrides the Forgejo default");
    let redmine: Vec<String> = ["--provider", "redmine", "issue", "get", "7"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (redmine_exit, _stdout, stderr) = run_captured(fixture.repo.path(), &redmine);
    assert_ne!(redmine_exit, 0, "the Redmine route is the one that runs");
    assert!(
        stderr.to_ascii_lowercase().contains("redmine"),
        "the failure names Redmine, not Forgejo: {stderr}"
    );
    let requests = mock.finish();
    assert!(
        requests.is_empty(),
        "no override reaches the Forgejo default: {requests:?}"
    );
}

/// Issue 1201 P13 B4: origin evidence participates only through a normalized
/// host and deployment prefix; transport, port and the endpoint itself never
/// come from Git, and unsafe origins refuse instead of falling back.
#[test]
fn origin_attests_only_the_trusted_host_and_prefix() {
    let fixture = Fixture::new("origin-attestation");
    let storage = fixture.attested_storage();
    storage
        .save_forgejo_auth(
            Role::Orchestrator,
            "https://forgejo.cloud1ful.com/team",
            "owner/repo",
            "fixture-token",
        )
        .unwrap();
    let resolve = |dir: &Path, repository: Option<&str>| {
        in_dir(dir, || {
            crate::providers::forgejo::ForgejoConfig::resolve(Role::Orchestrator, None, repository)
        })
    };
    let cases = [
        (
            "ssh-port",
            Some("ssh://git@forgejo.cloud1ful.com:2224/team/group/alpha.git"),
            Some("group/alpha"),
        ),
        (
            "scp",
            Some("git@forgejo.cloud1ful.com:team/group/alpha.git"),
            Some("group/alpha"),
        ),
        (
            "https",
            Some("https://forgejo.cloud1ful.com/team/group/alpha"),
            Some("group/alpha"),
        ),
        (
            "foreign-host",
            Some("https://other.test/team/group/alpha"),
            None,
        ),
        (
            "other-prefix",
            Some("https://forgejo.cloud1ful.com/other/group/alpha"),
            None,
        ),
        (
            "traversal",
            Some("https://forgejo.cloud1ful.com/team/../evil/group/alpha"),
            None,
        ),
        ("malformed", Some("not a url"), None),
        ("missing", None, None),
    ];
    for (label, origin, expected) in cases {
        let directory = checkout(label, origin);
        let result = resolve(directory.path(), None);
        match expected {
            Some(repository) => {
                let config = result.unwrap_or_else(|error| panic!("{label}: {error:?}"));
                assert_eq!(config.api_base(), "https://forgejo.cloud1ful.com/team");
                assert_eq!(config.repository(), repository);
            }
            None => assert!(result.is_err(), "{label} must refuse: {result:?}"),
        }
    }
    let ambiguous = checkout(
        "origin-ambiguous-review",
        Some("https://forgejo.cloud1ful.com/team/group/alpha"),
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "set-url",
                "--add",
                "origin",
                "https://forgejo.cloud1ful.com/team/group/beta"
            ])
            .current_dir(ambiguous.path())
            .status()
            .unwrap()
            .success()
    );
    let result = resolve(ambiguous.path(), None);
    assert!(result.is_err(), "multiple origins must refuse: {result:?}");
    let foreign = checkout(
        "origin-explicit",
        Some("https://other.test/team/group/alpha"),
    );
    let config = resolve(foreign.path(), Some("group/alpha"))
        .expect("an explicit repository bypasses origin selection, not instance trust");
    assert_eq!(config.api_base(), "https://forgejo.cloud1ful.com/team");
    assert_eq!(config.repository(), "group/alpha");

    let outside = TempDir::new("origin-outside-git");
    let config = resolve(outside.path(), None).expect("outside Git keeps the stored default");
    assert_eq!(config.api_base(), "https://forgejo.cloud1ful.com/team");
    assert_eq!(config.repository(), "owner/repo");
}

/// Issue 1201 P13 B3: a credential without a trusted instance, and an explicit
/// endpoint that does not normalize to the trusted one, both refuse before any
/// request can carry the role token.
#[test]
fn insecure_instance_selection_refuses_before_any_request() {
    {
        let fixture = Fixture::new("instance-no-trust");
        fixture.storage();
        let missing = fixture.zero_mock();
        let args: Vec<String> = [
            "--provider",
            "forgejo",
            "--api-base",
            &missing.base,
            "--repository",
            "owner/repo",
            "issue",
            "get",
            "7",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_rejected(fixture.repo.path(), "orchestrator", &args, missing);
    }

    let fixture = Fixture::new("instance-override");
    fixture.attested_storage();
    let mock = fixture.zero_mock();
    let args: Vec<String> = [
        "--provider",
        "forgejo",
        "--api-base",
        "https://other.test",
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
fn own_task_closure_reads_both_same_number_tasks_across_invalidation() {
    use crate::cli::issue_closure::{IssueStateSource, TaskScopeRef};

    let fixture = Fixture::new("own-task-identity");
    let storage = fixture.attested_storage();
    let journals = RedmineJournals::new();
    let mock = PathMock::start(redmine_issue_rules(
        63,
        (5, "Closed".to_owned()),
        (5, "Closed".to_owned()),
        &journals,
    ));
    storage
        .save_global_setting("PHASEGENT_REDMINE_API_BASE", &mock.base)
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-key")
        .unwrap();
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "local");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let closed = bind_redmine_task(&storage, &fixture, &mock.base, "p1", 63);
    let local_path = fixture.db.path().join("local.sqlite3");
    let local = crate::infra::local_store::open_local_at(&local_path).unwrap();
    local
        .connection
        .execute(
            "INSERT INTO local_issues (id,title,body,status,project,tracker,author_role,created_at,updated_at,closed_at) VALUES (63,'open','','Resolved','default','Task','executor',1,1,NULL)",
            [],
        )
        .unwrap();
    drop(local);
    let local_key = crate::task_identity::TaskKey::new("local", "local", "default", "63").unwrap();
    let open = crate::task_identity::ensure_binding(&storage, &local_key, 10).unwrap();
    crate::task_identity::add_repo_membership(
        &storage,
        &route_repo_key(&fixture),
        open.task_id(),
        10,
    )
    .unwrap();

    let source = crate::cli::issue_closure::configured_source_task_aware(
        Role::Orchestrator,
        crate::cli::provider_context::ProviderContext::default(),
    );
    let closed_instance = closed.key().instance().to_owned();
    let open_instance = open.key().instance().to_owned();
    let read = |task_id: i64, provider: &str, project: &str, instance: &str| {
        source.own_task_closure(
            fixture.repo.path(),
            TaskScopeRef {
                task_id,
                provider,
                project,
                instance,
            },
            63,
        )
    };

    assert!(
        read(closed.task_id(), "redmine", "p1", &closed_instance).is_closed(),
        "the closed task reads closed"
    );
    assert!(
        !read(open.task_id(), "local", "default", &open_instance).is_closed(),
        "the open same-number task must not receive the closed task's cached verdict"
    );

    source.refresh();
    assert!(
        !read(open.task_id(), "local", "default", &open_instance).is_closed(),
        "invalidation must not alias the open task onto the closed one"
    );
    assert!(
        read(closed.task_id(), "redmine", "p1", &closed_instance).is_closed(),
        "the closed task still reads closed after invalidation"
    );
    let requests = mock.finish();
    assert!(
        requests
            .iter()
            .filter(|(_, path, _)| path.starts_with("/issues/63.json"))
            .count()
            >= 2,
        "both reads of the closed task reach its own instance: {requests:?}"
    );
}

#[test]
fn an_unscoped_legacy_association_keeps_without_consulting_the_invocation_provider() {
    use crate::lifecycle::cleanup::closure::{DirectoryAssociation, DirectoryIssueClosure};

    let fixture = Fixture::new("unscoped-legacy-closure");
    let _storage = fixture.attested_storage();
    let mock = ZeroMock::start();
    let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "redmine");
    let _config = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        fixture.db.path().join("missing.toml").to_str().unwrap(),
    );
    let closure = crate::cli::issue_closure::CurrentIssueClosure::new_context(
        fixture.repo.path(),
        Role::Orchestrator,
        crate::cli::provider_context::ProviderContext::default(),
    );
    let unscoped = crate::worktree::DirectoryLegacyIssue {
        issue: 7,
        provider: String::new(),
        project: String::new(),
        external_id: 0,
    };
    let error = closure
        .confirm_association_closed(99, DirectoryAssociation::Legacy(&unscoped), None)
        .expect_err("an unknown legacy row keeps");
    assert!(
        error.contains("no corroborated provider/project/instance provenance"),
        "got: {error}"
    );
    assert!(
        mock.finish().is_empty(),
        "the invocation default must not be read for an unknown association"
    );
}

#[test]
fn a_redmine_sidecar_without_a_durable_task_keeps_without_reading_the_configured_endpoint() {
    let fixture = Fixture::new("prune-redmine-no-task");
    let storage = fixture.attested_storage();
    crate::branch_links::ensure_schema(&storage.connection).unwrap();
    let directory = fixture.worktree("phasegent/7-redmine-no-task");
    let scope = WorktreeScope::new("redmine", "23", 7).unwrap();
    fixture.lease(
        &storage,
        "redmine-no-task",
        7,
        &directory,
        "phasegent/7-redmine-no-task",
        "retained",
        Some(&scope),
    );
    let trusted = ZeroMock::start();
    storage
        .save_global_setting("PHASEGENT_REDMINE_API_BASE", &trusted.base)
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-key")
        .unwrap();
    let _unset = UnsetEnv::apply(&["PHASEGENT_REDMINE_API_BASE"]);
    let repo = fixture.repo.path().to_string_lossy().to_string();
    let args: Vec<String> = [
        "--provider",
        "redmine",
        "--project-id",
        "23",
        "--api-base",
        &trusted.base,
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
    assert_eq!(exit, 0);
    assert!(
        trusted.finish().is_empty(),
        "the configured Redmine endpoint is not historical proof for a sidecar with no task"
    );
    assert!(
        directory.exists(),
        "a scope-only sidecar without an immutable task keeps the directory"
    );
}

#[test]
fn a_same_number_current_redmine_scope_never_proves_an_unbound_legacy_sidecar() {
    use crate::cli::issue_closure::{CurrentIssueClosure, ExplicitSelection};
    use crate::lifecycle::cleanup::closure::{DirectoryAssociation, DirectoryIssueClosure};

    let fixture = Fixture::new("redmine-current-scope-unbound");
    let storage = fixture.attested_storage();
    let old_directory = fixture.worktree("phasegent/7-old-instance");
    let old_scope = WorktreeScope::new("redmine", "23", 7).unwrap();
    fixture.lease(
        &storage,
        "old-instance",
        7,
        &old_directory,
        "phasegent/7-old-instance",
        "retained",
        Some(&old_scope),
    );

    // The currently configured trusted instance B reports issue 7 closed, but
    // the sidecar records no instance and no durable task. Nothing may
    // synthesize B as the historical instance A of the old directory.
    let journals = RedmineJournals::new();
    let current = PathMock::start(redmine_issue_rules(
        7,
        (5, "Closed".to_owned()),
        (5, "Closed".to_owned()),
        &journals,
    ));
    storage
        .save_global_setting("PHASEGENT_REDMINE_API_BASE", &current.base)
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", "fixture-key")
        .unwrap();
    let _unset = UnsetEnv::apply(&["PHASEGENT_REDMINE_API_BASE"]);

    let closure = CurrentIssueClosure::new_scoped(
        fixture.repo.path(),
        Role::Orchestrator,
        ExplicitSelection {
            provider_kind: crate::providers::ProviderKind::Redmine,
            api_base: Some(current.base.clone()),
            repository: None,
            project_id: Some("23".to_owned()),
            close_status_id: Some("5".to_owned()),
        },
    );
    let legacy = crate::worktree::DirectoryLegacyIssue {
        issue: 7,
        provider: "redmine".to_owned(),
        project: "23".to_owned(),
        external_id: 7,
    };
    let current_scope = WorktreeScope::new("redmine", "23", 7).unwrap();
    let error = closure
        .confirm_association_closed(
            7,
            DirectoryAssociation::Legacy(&legacy),
            Some(&current_scope),
        )
        .expect_err("current instance B cannot prove the unbound historical sidecar");
    assert!(
        error.contains("no corroborating durable task"),
        "got: {error}"
    );
    assert!(
        current.finish().is_empty(),
        "the current endpoint must not be read as historical proof"
    );
    assert!(
        old_directory.exists(),
        "the old directory survives the current same-scope close"
    );
}
