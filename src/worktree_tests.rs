//! Tests for the worktree leasing core (issue #239 Phase 1).
//!
//! Coverage spans the four pieces the Phase 1 scope calls out:
//!
//! * **Helpers** (`validate_ref_format`, `compute_fingerprint`,
//!   `slug_from_branch`, `generate_branch`, `parse_worktree_list`,
//!   `repo_identity`, `cache_root_in`) — pure-string / pure-path
//!   unit tests with a `FakeWorktreeRunner` for the git-driven
//!   pieces.
//! * **Storage** (`ensure_schema`, idempotent create, lease round
//!   trip) — exercises the `CREATE TABLE IF NOT EXISTS` migration
//!   through a real `Storage::open_at` against a temp
//!   `PHASEGENT_DB_PATH` so the operator's real database is never
//!   touched.
//! * **Acquisition flows** — idempotent reuse, `no_conflict`
//!   reuse-current-checkout, `new_worktree` for a second session, and
//!   cross-issue isolation. Each test runs in its own temp git repo
//!   under `/tmp` and never mutates the real repository's worktrees.
//! * **Release / dirty probe** — `release_lease` flips the row to
//!   `retained`/`released`, and `is_clean` returns the documented
//!   `Ok(true)` / `Ok(false)` / structured-error values against real
//!   git.

use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::worktree::{
    AcquireOutcome, GitOutput, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED,
    ProcessWorktreeRunner, WorktreeError, WorktreeListEntry, WorktreeRunner, acquire_lease,
    cache_root_in, compute_fingerprint, generate_branch, is_clean, leases_for_issue,
    leases_for_repo, parse_worktree_list, release_lease, repo_identity, slug_from_branch,
    validate_ref_format, worktree_add, worktree_remove,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Test helpers: fake runner, temp repo, temp DB override.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct FakeResponse {
    /// Matched as a prefix of the actual argv (after `git`).
    args: Vec<String>,
    status: i32,
    stdout: String,
}

struct FakeWorktreeRunner {
    responses: RefCell<Vec<FakeResponse>>,
    calls: RefCell<Vec<(Vec<String>, PathBuf)>>,
}

impl FakeWorktreeRunner {
    fn new(responses: Vec<FakeResponse>) -> Self {
        Self {
            responses: RefCell::new(responses),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn recorded(&self) -> Vec<(Vec<String>, PathBuf)> {
        self.calls.borrow().clone()
    }
}

impl WorktreeRunner for FakeWorktreeRunner {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError> {
        let argv: Vec<String> = args.iter().map(|value| value.to_string()).collect();
        self.calls
            .borrow_mut()
            .push((argv.clone(), workdir.to_path_buf()));
        for response in self.responses.borrow().iter() {
            if argv.starts_with(&response.args) {
                return Ok(GitOutput {
                    status: response.status,
                    stdout: response.stdout.clone(),
                });
            }
        }
        Err(WorktreeError::new(
            "git",
            format!("unexpected git invocation {argv:?}"),
        ))
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "phasegent-wt-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir create");
        Self(dir)
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

struct TempRepo {
    dir: TempDir,
    head_branch: String,
}

impl TempRepo {
    fn init(label: &str) -> Option<Self> {
        let dir = TempDir::new(label);
        let runner = ProcessWorktreeRunner::new();
        if runner
            .run(&["init", "-q", "-b", "main"], dir.path())
            .is_err()
        {
            return None;
        }
        // Configure a committer identity so the initial commit works
        // even in a host with no global git config.
        let _ = runner.run(
            &[
                "-c",
                "user.name=phasegent-test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
            dir.path(),
        );
        // If the initial commit failed (e.g. test runs in a CI without
        // user setup), the test still has a valid repo we can reason
        // about: `git worktree add` falls back to creating an empty
        // branch when HEAD is unborn. We just record whatever branch
        // `symbolic-ref` reports.
        let branch = match runner.run(&["symbolic-ref", "--quiet", "--short", "HEAD"], dir.path()) {
            Ok(out) if out.status == 0 => out.stdout.trim().to_string(),
            _ => "main".to_string(),
        };
        Some(Self {
            dir,
            head_branch: branch,
        })
    }
}

fn open_temp_db(label: &str) -> (TempDir, Storage, EnvGuard) {
    let temp = TempDir::new(label);
    let db = temp.path().join("phasegent.sqlite3");
    let env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db).expect("storage open");
    (temp, storage, env)
}

fn unique_cache(label: &str) -> TempDir {
    TempDir::new(&format!("cache-{label}"))
}

// ---------------------------------------------------------------------------
// Pure helper tests (no DB, no git).
// ---------------------------------------------------------------------------

#[test]
fn validate_ref_format_accepts_documented_branch_shape() {
    for input in [
        "phasegent/239-abcdef",
        "phasegent/1-aaaaaa",
        "main",
        "feature/x",
        "release/2024-01",
        "x",
    ] {
        assert!(
            validate_ref_format(input).is_ok(),
            "input {input:?} must pass"
        );
    }
}

#[test]
fn validate_ref_format_rejects_disallowed_characters() {
    for input in [
        "",                       // empty
        "PhaseGent/239-aaa",      // uppercase
        "phasegent/.239-aaa",     // leading dot inside segment
        "phasegent/239-aaa..",    // trailing ..
        "phasegent/239-aaa@{",    // @{ sequence
        "phasegent/239-aaa\\x",   // backslash
        "-leading-dash",          // leading dash
        "trailing-dot.",          // trailing dot
        "trailing.lock",          // trailing .lock
        "trailing/slash/",        // trailing slash
        "phasegent/239-aaa..bbb", // .. segment
        "phasegent/239-aaa bbb",  // embedded space
        "phasegent/239-aaa~1",    // ~ (not in our allowlist)
        "phasegent/239-aaa^1",    // ^
        "phasegent/239-aaa:1",    // :
        "phasegent/239-aaa?1",    // ?
        "phasegent/239-aaa*1",    // *
        "phasegent/239-aaa[1]",   // [
    ] {
        let error = validate_ref_format(input)
            .err()
            .unwrap_or_else(|| panic!("input {input:?} should have failed"));
        assert_eq!(error.kind, "argument", "input {input:?}");
    }
}

#[test]
fn validate_ref_format_rejects_oversized_names() {
    let oversized = "a".repeat(129);
    let error = validate_ref_format(&oversized).unwrap_err();
    assert_eq!(error.kind, "argument");
}

#[test]
fn compute_fingerprint_is_deterministic_and_distinguishes_repos() {
    let a = compute_fingerprint("/home/dev/repo-a/.git");
    let b = compute_fingerprint("/home/dev/repo-b/.git");
    let a2 = compute_fingerprint("/home/dev/repo-a/.git");
    assert_eq!(a, a2, "fingerprint must be stable across calls");
    assert_ne!(a, b, "different repos must produce different fingerprints");
    assert_eq!(a.len(), 12);
    assert!(
        a.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
}

#[test]
fn generate_branch_produces_phasegent_namespaced_short_hex_branches() {
    for issue in [1, 42, 239, 999_999] {
        let (branch, short) = generate_branch(issue).expect("generate");
        assert!(branch.starts_with(&format!("phasegent/{issue}-")));
        assert_eq!(short.len(), 6);
        assert!(short.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(validate_ref_format(&branch).is_ok());
    }
}

#[test]
fn slug_from_branch_replaces_slash_with_dash() {
    assert_eq!(
        slug_from_branch("phasegent/239-abcdef").unwrap(),
        "phasegent-239-abcdef"
    );
    assert_eq!(slug_from_branch("main").unwrap(), "main");
    let error = slug_from_branch("PhaseGent/239-abcdef").unwrap_err();
    assert_eq!(error.kind, "argument");
}

#[test]
fn parse_worktree_list_handles_porcelain_blocks_with_extra_fields() {
    let raw = "\
worktree /home/dev/repo
HEAD abcdef1234567890
branch refs/heads/main

worktree /home/dev/repo-wt
HEAD 0123456789abcdef
branch refs/heads/phasegent/239-abcdef
detached

";
    let entries: Vec<WorktreeListEntry> = parse_worktree_list(raw);
    assert_eq!(
        entries,
        vec![
            WorktreeListEntry {
                worktree: "/home/dev/repo".to_owned(),
                head: Some("abcdef1234567890".to_owned()),
                branch: Some("refs/heads/main".to_owned()),
            },
            WorktreeListEntry {
                worktree: "/home/dev/repo-wt".to_owned(),
                head: Some("0123456789abcdef".to_owned()),
                branch: Some("refs/heads/phasegent/239-abcdef".to_owned()),
            },
        ]
    );
}

#[test]
fn parse_worktree_list_returns_empty_for_empty_input() {
    assert!(parse_worktree_list("").is_empty());
    assert!(parse_worktree_list("\n\n\n").is_empty());
}

// ---------------------------------------------------------------------------
// repo_identity via FakeWorktreeRunner.
// ---------------------------------------------------------------------------

#[test]
fn repo_identity_canonicalises_relative_and_absolute_common_dirs() {
    let repo_path = PathBuf::from("/tmp/some/repo");
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["rev-parse".to_string(), "--git-common-dir".to_string()],
        status: 0,
        stdout: ".git".to_string(),
    }]);
    let identity = repo_identity(&runner, &repo_path).expect("identity");
    assert!(identity.ends_with("/repo/.git") || identity.ends_with("/repo"));
    let recorded = runner.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, vec!["rev-parse", "--git-common-dir"]);
    assert_eq!(recorded[0].1, repo_path);
}

#[test]
fn repo_identity_reports_structured_git_failure() {
    let repo_path = PathBuf::from("/tmp/some/repo");
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["rev-parse".to_string(), "--git-common-dir".to_string()],
        status: 128,
        stdout: "fatal: not a git repo".to_string(),
    }]);
    let error = repo_identity(&runner, &repo_path).unwrap_err();
    assert_eq!(error.kind, "git");
    assert!(!error.message.is_empty());
}

// ---------------------------------------------------------------------------
// Schema and storage tests (real SQLite via Storage::open_at).
// ---------------------------------------------------------------------------

#[test]
fn ensure_schema_is_idempotent_and_creates_expected_columns() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("schema");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema first call");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema second call");
    let mut statement = storage
        .connection
        .prepare("PRAGMA table_info(worktree_leases)")
        .expect("pragma");
    let columns: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query")
        .filter_map(|row| row.ok())
        .collect();
    for expected in [
        "lease_id",
        "repo_identity",
        "issue",
        "session",
        "checkout_path",
        "worktree_path",
        "branch",
        "status",
        "created_at",
        "heartbeat_at",
    ] {
        assert!(
            columns.contains(&expected.to_owned()),
            "missing column {expected}"
        );
    }
    drop(temp);
}

#[test]
fn leases_for_issue_and_repo_return_only_matching_active_rows() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("list");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    let now = crate::worktree::now_unix_secs();
    let identity = "/tmp/repo-1";
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-a",
            identity,
            issue: 239,
            session: "session-1",
            checkout_path: "/tmp/repo-1",
            worktree_path: "/tmp/repo-1/wt-a",
            branch: "phasegent/239-aaaaaa",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("insert a");
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-b",
            identity,
            issue: 239,
            session: "session-2",
            checkout_path: "/tmp/repo-1",
            worktree_path: "/tmp/repo-1/wt-b",
            branch: "phasegent/239-bbbbbb",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("insert b");
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-c",
            identity: "/tmp/repo-2",
            issue: 239,
            session: "session-1",
            checkout_path: "/tmp/repo-2",
            worktree_path: "/tmp/repo-2",
            branch: "phasegent/239-cccccc",
            status: LEASE_STATUS_RETAINED,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("insert c");
    let issue_rows = leases_for_issue(239).expect("issue list");
    assert_eq!(issue_rows.len(), 2);
    assert!(
        issue_rows
            .iter()
            .all(|row| row.status == LEASE_STATUS_ACTIVE)
    );
    assert!(issue_rows.iter().all(|row| row.issue == 239));
    let repo_rows = leases_for_repo(identity).expect("repo list");
    assert_eq!(repo_rows.len(), 2);
    assert!(repo_rows.iter().all(|row| row.repo_identity == identity));
    drop(temp);
}

#[test]
fn unique_index_blocks_duplicate_repo_path_rows() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("unique");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    let now = crate::worktree::now_unix_secs();
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-dup",
            identity: "/tmp/repo",
            issue: 1,
            session: "s",
            checkout_path: "/tmp/repo",
            worktree_path: "/tmp/repo",
            branch: "phasegent/1-aaaaaa",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("first insert");
    let result = crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-dup-2",
            identity: "/tmp/repo",
            issue: 2,
            session: "s",
            checkout_path: "/tmp/repo",
            worktree_path: "/tmp/repo",
            branch: "phasegent/2-bbbbbb",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    );
    assert!(
        result.is_err(),
        "duplicate (repo_identity, worktree_path) must error"
    );
    drop(temp);
}

// ---------------------------------------------------------------------------
// Cache root test (filesystem-only).
// ---------------------------------------------------------------------------

#[test]
fn cache_root_in_creates_private_dir_under_the_provided_base() {
    let base = unique_cache("root");
    let fingerprint = "abcdef123456";
    let dir = cache_root_in(base.path(), fingerprint).expect("cache root");
    assert!(dir.exists());
    assert!(dir.ends_with("worktrees/abcdef123456"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "cache dir must be 0700");
    }
    drop(base);
}

// ---------------------------------------------------------------------------
// Real-git tests. Each test runs in its own temp repo so the
// production / current worktree tree is never mutated.
// ---------------------------------------------------------------------------

#[test]
fn acquire_reuses_current_checkout_when_no_other_lease_is_active() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("no-conflict") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-no-conflict");
    let cache = unique_cache("acquire-no-conflict");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
    )
    .expect("acquire no_conflict");
    let expected_path = repo.dir.path().to_string_lossy().to_string();
    assert_eq!(outcome.reason, "no_conflict");
    assert!(!outcome.created);
    assert_eq!(outcome.path, expected_path);
    assert_eq!(outcome.branch, repo.head_branch);
    assert!(!outcome.lease_id.is_empty());
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_is_idempotent_for_the_same_repo_issue_session() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("idempotent") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-idempotent");
    let cache = unique_cache("acquire-idempotent");
    let runner = ProcessWorktreeRunner::new();
    let first = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
    )
    .expect("first acquire");
    let second = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
    )
    .expect("second acquire");
    assert_eq!(
        first.lease_id, second.lease_id,
        "idempotent acquire must reuse the lease"
    );
    assert_eq!(first.path, second.path);
    assert_eq!(first.branch, second.branch);
    assert_eq!(second.reason, "idempotent");
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_creates_a_new_worktree_for_a_second_session() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("new-worktree") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-new");
    let cache = unique_cache("acquire-new");
    let runner = ProcessWorktreeRunner::new();
    let first = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
    )
    .expect("first acquire");
    assert_eq!(first.reason, "no_conflict");
    let second = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-B",
        Some(cache.path()),
    )
    .expect("second acquire");
    assert_eq!(second.reason, "new_worktree");
    assert!(second.created);
    assert_ne!(second.lease_id, first.lease_id);
    assert_ne!(second.path, first.path);
    assert!(
        second
            .path
            .starts_with(cache.path().to_string_lossy().as_ref())
    );
    assert!(second.branch.starts_with("phasegent/239-"));
    assert!(Path::new(&second.path).exists());
    let third = acquire_lease(
        &runner,
        repo.dir.path(),
        240,
        "session-A",
        Some(cache.path()),
    )
    .expect("third acquire");
    assert_eq!(third.reason, "new_worktree");
    assert!(third.branch.starts_with("phasegent/240-"));
    drop(cache);
    drop(db_temp);
}

#[test]
fn release_flips_lease_to_retained_or_released_and_is_idempotent() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("release") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("release");
    let cache = unique_cache("release");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
    )
    .expect("acquire");
    let retain = release_lease(&outcome.lease_id, true).expect("release retain");
    assert_eq!(retain.status, LEASE_STATUS_RETAINED);
    let second_retain = release_lease(&outcome.lease_id, true).expect("release retain again");
    assert_eq!(second_retain.status, LEASE_STATUS_RETAINED);
    let missing = release_lease("lease-does-not-exist", true);
    let error = missing.expect_err("missing lease must error");
    assert_eq!(error.kind, "state");
    drop(cache);
    drop(db_temp);
}

#[test]
fn release_can_flip_a_lease_to_released() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("release-non-retain") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("release-non-retain");
    let cache = unique_cache("release-non-retain");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
    )
    .expect("acquire");
    let released = release_lease(&outcome.lease_id, false).expect("release released");
    assert_eq!(released.status, LEASE_STATUS_RELEASED);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_rejects_zero_issue_and_oversized_session() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("acquire-args") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-args");
    let cache = unique_cache("acquire-args");
    let runner = ProcessWorktreeRunner::new();
    let error = acquire_lease(&runner, repo.dir.path(), 0, "session", Some(cache.path()))
        .expect_err("zero issue must error");
    assert_eq!(error.kind, "argument");
    let huge = "x".repeat(129);
    let error = acquire_lease(&runner, repo.dir.path(), 239, &huge, Some(cache.path()))
        .expect_err("oversized session must error");
    assert_eq!(error.kind, "argument");
    drop(cache);
    drop(db_temp);
}

#[test]
fn worktree_add_and_remove_create_then_drop_a_real_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("wt-add-remove") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join("wt-test");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/239-aaaaaa").expect("worktree add");
    assert!(target.exists());
    let porcelain = Command::new("git")
        .arg("-C")
        .arg(target.to_string_lossy().to_string())
        .args(["status", "--porcelain"])
        .output();
    if let Ok(out) = porcelain {
        assert_eq!(out.status.code().unwrap_or(-1), 0);
    }
    worktree_remove(&runner, repo.dir.path(), &target).expect("worktree remove");
    assert!(!target.exists());
}

#[test]
fn is_clean_returns_true_for_pristine_and_false_for_dirty() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("is-clean") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    assert!(is_clean(&runner, repo.dir.path()).expect("clean check"));
    let dirty = repo.dir.path().join("scratch.txt");
    std::fs::write(&dirty, "scratch\n").expect("write scratch");
    assert!(!is_clean(&runner, repo.dir.path()).expect("dirty check"));
    let _ = std::fs::remove_file(&dirty);
}

#[test]
fn is_clean_reports_structured_error_for_git_failure() {
    let _lock = lock_workflow_tests();
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["status".to_string(), "--porcelain".to_string()],
        status: 128,
        stdout: "fatal: not a git repo".to_string(),
    }]);
    let error = is_clean(&runner, Path::new("/tmp/nope")).unwrap_err();
    assert_eq!(error.kind, "git");
}

#[test]
fn worktree_add_surfaces_git_failure_as_structured_error() {
    let _lock = lock_workflow_tests();
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["worktree".to_string(), "add".to_string()],
        status: 128,
        stdout: "fatal: bad ref".to_string(),
    }]);
    let error = worktree_add(
        &runner,
        Path::new("/tmp/repo"),
        Path::new("/tmp/repo/wt"),
        "phasegent/1-aaaaaa",
    )
    .unwrap_err();
    assert_eq!(error.kind, "git");
}

#[test]
fn acquire_lease_outcome_serialises_required_fields() {
    let outcome = AcquireOutcome {
        lease_id: "lease-x".to_owned(),
        path: "/tmp/p".to_owned(),
        branch: "phasegent/1-aaaaaa".to_owned(),
        repo_identity: "/tmp/r/.git".to_owned(),
        created: true,
        reason: "new_worktree".to_owned(),
    };
    assert_eq!(outcome.lease_id, "lease-x");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
}
