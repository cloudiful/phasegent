use super::support::*;
use super::*;

use crate::worktree::{AcquireOptions, acquire_lease_with};

/// Run `git <args>` in `dir` and return trimmed stdout, asserting exit 0.
fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let runner = ProcessWorktreeRunner::new();
    let output = runner.run(args, dir).expect("git runs");
    assert_eq!(output.status, 0, "git {args:?} must succeed");
    output.stdout.trim().to_owned()
}

#[test]
fn acquire_explicit_base_creates_a_fresh_worktree_from_the_ref() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("base-new") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-base-new");
    let cache = unique_cache("acquire-base-new");
    let runner = ProcessWorktreeRunner::new();
    let head = git_stdout(repo.dir.path(), &["rev-parse", "HEAD"]);
    let outcome = acquire_lease_with(
        &runner,
        repo.dir.path(),
        595,
        "session-A",
        AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: false,
            auto: false,
            base: Some("main"),
        },
    )
    .expect("explicit base must create a worktree");
    assert!(
        outcome.created,
        "an explicit base never reuses the checkout"
    );
    assert_eq!(outcome.reason, "new_worktree");
    assert!(outcome.branch.starts_with("phasegent/595-"));
    assert!(
        outcome
            .path
            .starts_with(cache.path().to_string_lossy().as_ref()),
        "the worktree must live under the cache base"
    );
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("--base 'main'"),
        "the explicit base must be visible in the warnings: {joined}"
    );
    let target = Path::new(&outcome.path);
    assert!(target.exists(), "the worktree directory must exist");
    assert_eq!(
        git_stdout(target, &["rev-parse", "HEAD"]),
        head,
        "the new worktree must start at the requested base commit"
    );
    // The one-command closure is unchanged: the new branch is bound to
    // the acquired issue exactly like the HEAD-based path.
    let git_runner = crate::branch_context::ProcessGitRunner::in_directory(target);
    let bound = crate::branch_context::read_issue_id(&git_runner, &outcome.branch)
        .expect("binding read in the base worktree");
    assert_eq!(bound, Some(595), "the base worktree branch must be bound");
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let rows = leases_for_repo(&identity).expect("list leases");
    assert_eq!(rows.len(), 1, "exactly one lease must be recorded");
    assert_eq!(rows[0].branch, outcome.branch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_base_honours_an_older_ref_not_just_head() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("base-older") else {
        return;
    };
    let first = git_stdout(repo.dir.path(), &["rev-parse", "HEAD"]);
    let scratch = repo.dir.path().join("second.txt");
    std::fs::write(&scratch, "second\n").expect("write second");
    git_stdout(repo.dir.path(), &["add", "second.txt"]);
    git_stdout(
        repo.dir.path(),
        &[
            "-c",
            "user.name=phasegent-test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "second",
        ],
    );
    let second = git_stdout(repo.dir.path(), &["rev-parse", "HEAD"]);
    assert_ne!(first, second, "the fixture needs two distinct commits");

    let (db_temp, _storage, _env) = open_temp_db("acquire-base-older");
    let cache = unique_cache("acquire-base-older");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease_with(
        &runner,
        repo.dir.path(),
        595,
        "session-A",
        AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: false,
            auto: false,
            base: Some(&first),
        },
    )
    .expect("an older ref must be honoured");
    assert_eq!(
        git_stdout(Path::new(&outcome.path), &["rev-parse", "HEAD"]),
        first,
        "the worktree must be based on the requested ref, not HEAD"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_base_is_idempotent_first_for_the_same_triple() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("base-idempotent") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-base-idempotent");
    let cache = unique_cache("acquire-base-idempotent");
    let runner = ProcessWorktreeRunner::new();
    let first = acquire_lease(
        &runner,
        repo.dir.path(),
        595,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("first acquire reuses the checkout");
    assert_eq!(first.reason, "no_conflict");
    // The same triple with a bogus --base must still be idempotent: the
    // home-coming check runs before any base validation or worktree work.
    let second = acquire_lease_with(
        &runner,
        repo.dir.path(),
        595,
        "session-A",
        AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: false,
            auto: false,
            base: Some("definitely-not-a-ref"),
        },
    )
    .expect("the idempotent home-coming must win over --base");
    assert!(!second.created);
    assert_eq!(second.reason, "idempotent");
    assert_eq!(second.lease_id, first.lease_id);
    assert_eq!(second.path, first.path);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_bad_base_fails_locally_without_lease_or_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("base-bad") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-base-bad");
    let cache = unique_cache("acquire-base-bad");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let error = acquire_lease_with(
        &runner,
        repo.dir.path(),
        595,
        "session-A",
        AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: false,
            auto: false,
            base: Some("no-such-ref"),
        },
    )
    .expect_err("a bad base must fail before any creation");
    assert_eq!(error.kind, "git");
    assert!(
        error.message.contains("no-such-ref"),
        "the error must name the bad ref: {error}"
    );
    let rows = leases_for_repo(&identity).expect("list leases");
    assert!(
        rows.is_empty(),
        "a bad base must not leave a half lease: {rows:?}"
    );
    let branches = git_stdout(repo.dir.path(), &["branch", "--list", "phasegent/595-*"]);
    assert!(
        branches.is_empty(),
        "a bad base must not leave a half branch: {branches}"
    );
    assert!(
        !cache.path().join("worktrees").exists(),
        "a bad base must not create a worktree directory"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_without_base_keeps_the_existing_decision_table() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("base-none") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-base-none");
    let cache = unique_cache("acquire-base-none");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease_with(
        &runner,
        repo.dir.path(),
        595,
        "session-A",
        AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: false,
            auto: false,
            base: None,
        },
    )
    .expect("no base reuses the clean checkout");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    drop(cache);
    drop(db_temp);
}
