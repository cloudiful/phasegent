use super::support::*;
use super::*;

#[test]
fn acquire_binding_read_failure_falls_through_without_error() {
    // Binding resolution is best-effort: when the fake runner reports
    // the checkout dirty and the real binding read then fails (the
    // path is not a git checkout), acquire falls through to reuse
    // instead of hard-erroring, and the failure is surfaced as a
    // warning.
    let _lock = lock_workflow_tests();
    let (db_temp, _storage, _env) = open_temp_db("acquire-binding-failure");
    let cache = unique_cache("binding-failure");
    let no_repo = crate::test_scratch::root().join(format!(
        "phasegent-wt-no-repo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&no_repo);
    let runner = FakeWorktreeRunner::new(vec![
        FakeResponse {
            args: vec!["rev-parse".to_string(), "--git-common-dir".to_string()],
            status: 0,
            stdout: ".git".to_string(),
        },
        FakeResponse {
            args: vec!["status".to_string(), "--porcelain".to_string()],
            status: 0,
            stdout: " M scratch.txt".to_string(),
        },
        FakeResponse {
            args: vec![
                "symbolic-ref".to_string(),
                "--quiet".to_string(),
                "--short".to_string(),
                "HEAD".to_string(),
            ],
            status: 0,
            stdout: "main".to_string(),
        },
    ]);
    let outcome = acquire_lease(
        &runner,
        &no_repo,
        245,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("binding read failure must never hard-error the acquire");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert!(
        outcome.warnings.iter().any(|w| w.contains("unbound")),
        "binding read failure must be surfaced as a warning: {:?}",
        outcome.warnings
    );
    let _ = std::fs::remove_dir_all(&no_repo);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_git_status_failure_with_auto_isolation_creates_isolated_worktree() {
    // Issue 305 Task 4: a failing `git status` is an unknown state, not
    // a clean tree. With auto-isolation on we must not reuse the
    // untrusted checkout, so a fresh worktree is created.
    let _lock = lock_workflow_tests();
    let (db_temp, _storage, _env) = open_temp_db("acquire-unknown-auto");
    let cache = unique_cache("unknown-auto");
    let repo_path = PathBuf::from("/tmp/phasegent-wt-unknown-auto-repo");
    let runner = FakeWorktreeRunner::new(vec![
        FakeResponse {
            args: vec!["rev-parse".to_string(), "--git-common-dir".to_string()],
            status: 0,
            stdout: ".git".to_string(),
        },
        FakeResponse {
            args: vec!["status".to_string(), "--porcelain".to_string()],
            status: 128,
            stdout: "fatal: index file corrupt".to_string(),
        },
        FakeResponse {
            args: vec!["worktree".to_string(), "add".to_string()],
            status: 0,
            stdout: String::new(),
        },
    ]);
    let outcome = acquire_lease(
        &runner,
        &repo_path,
        245,
        "session-A",
        Some(cache.path()),
        true,
        false,
    )
    .expect("unknown state + auto-isolation must isolate, not error");
    assert!(outcome.created, "unknown checkout must not be reused");
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .path
            .starts_with(cache.path().to_string_lossy().as_ref()),
        "isolated worktree must live under the cache base"
    );
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("dirty probe failed")
            && joined.contains("auto-isolation is enabled")
            && joined.contains("unknown"),
        "unknown probe must be surfaced as a warning: {joined}"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_git_status_failure_with_auto_isolation_disabled_reuses_with_warning() {
    // Issue 305 Task 4: with isolation off the unknown checkout is
    // reused, but the operator must be warned rather than told the tree
    // is clean.
    let _lock = lock_workflow_tests();
    let (db_temp, _storage, _env) = open_temp_db("acquire-unknown-off");
    let cache = unique_cache("unknown-off");
    let repo_path = PathBuf::from("/tmp/phasegent-wt-unknown-off-repo");
    let runner = FakeWorktreeRunner::new(vec![
        FakeResponse {
            args: vec!["rev-parse".to_string(), "--git-common-dir".to_string()],
            status: 0,
            stdout: ".git".to_string(),
        },
        FakeResponse {
            args: vec!["status".to_string(), "--porcelain".to_string()],
            status: 128,
            stdout: "fatal: index file corrupt".to_string(),
        },
        FakeResponse {
            args: vec![
                "symbolic-ref".to_string(),
                "--quiet".to_string(),
                "--short".to_string(),
                "HEAD".to_string(),
            ],
            status: 0,
            stdout: "main".to_string(),
        },
    ]);
    let outcome = acquire_lease(
        &runner,
        &repo_path,
        245,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("unknown state + default off must reuse, not error");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo_path.to_string_lossy().to_string());
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("dirty probe failed")
            && joined.contains("auto-isolation is disabled")
            && joined.contains("unknown"),
        "default-off unknown state must warn instead of staying silent: {joined}"
    );
    assert!(
        !cache.path().join("worktrees").exists(),
        "default-off unknown state must not create a worktree directory"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_isolate_flag_wins_over_clean_checkout_with_retained_row() {
    // Issue #509 replica: clean checkout + one RETAINED row occupying the
    // current directory + `--isolate` must create a fresh worktree instead
    // of colliding on the `(repo_identity, worktree_path)` unique index.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("isolate-retained-row") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("acquire-isolate-retained-row");
    let cache = unique_cache("isolate-retained-row");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("repo identity");
    let checkout = repo.dir.path().to_string_lossy().to_string();
    let now = now_unix_secs();
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    insert_lease(
        &storage,
        NewLease {
            lease_id: "lease-retained-occupant",
            identity: &identity,
            issue: 1,
            session: "old-session",
            checkout_path: &checkout,
            worktree_path: &checkout,
            branch: &repo.head_branch,
            status: LEASE_STATUS_RETAINED,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("seed retained row on the current checkout");
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        508,
        "session-A",
        Some(cache.path()),
        true,
        false,
    )
    .expect("explicit --isolate must win over the retained occupant");
    assert_eq!(outcome.reason, "new_worktree");
    assert!(outcome.created);
    assert!(outcome.branch.starts_with("phasegent/508-"));
    assert!(Path::new(&outcome.path).exists(), "worktree dir must exist");
    assert_ne!(outcome.path, checkout);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_isolate_flag_forces_new_worktree_on_empty_table() {
    // Issue #509 document lock: `--isolate` forces a fresh branch/worktree
    // even when the checkout is clean and no lease exists.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("isolate-empty") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-isolate-empty");
    let cache = unique_cache("isolate-empty");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        509,
        "session-A",
        Some(cache.path()),
        true,
        false,
    )
    .expect("explicit --isolate on a clean empty table must isolate");
    assert_eq!(outcome.reason, "new_worktree");
    assert!(outcome.created);
    assert!(outcome.branch.starts_with("phasegent/509-"));
    assert!(Path::new(&outcome.path).exists(), "worktree dir must exist");
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_without_isolate_still_collides_on_retained_row() {
    // Reverse lock for issue #509: retained rows deliberately do NOT
    // trigger default auto-isolation, so without any flag the clean reuse
    // path still collides on the unique index and reports the guidance
    // (which is now true: adding the flag really helps).
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("no-isolate-retained-row") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("acquire-no-isolate-retained-row");
    let cache = unique_cache("no-isolate-retained-row");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("repo identity");
    let checkout = repo.dir.path().to_string_lossy().to_string();
    let now = now_unix_secs();
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    insert_lease(
        &storage,
        NewLease {
            lease_id: "lease-retained-occupant",
            identity: &identity,
            issue: 1,
            session: "old-session",
            checkout_path: &checkout,
            worktree_path: &checkout,
            branch: &repo.head_branch,
            status: LEASE_STATUS_RETAINED,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("seed retained row on the current checkout");
    let error = acquire_lease(
        &runner,
        repo.dir.path(),
        508,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect_err("no flag + retained occupant must still hit the repo/path conflict");
    assert_eq!(error.kind, "storage");
    assert!(
        error.message.contains("--isolate")
            && error.message.contains("worktree status")
            && error.message.contains("worktree list"),
        "default collision must keep the original guidance: {error}"
    );
    drop(cache);
    drop(db_temp);
}
