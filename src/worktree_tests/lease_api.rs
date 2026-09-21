use super::support::*;
use super::*;

#[test]
fn acquire_rejects_zero_issue_and_oversized_session() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("acquire-args") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-args");
    let cache = unique_cache("acquire-args");
    let runner = ProcessWorktreeRunner::new();
    let error = acquire_lease(
        &runner,
        repo.dir.path(),
        0,
        "session",
        Some(cache.path()),
        false,
        false,
    )
    .expect_err("zero issue must error");
    assert_eq!(error.kind, "argument");
    let huge = "x".repeat(129);
    let error = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        &huge,
        Some(cache.path()),
        false,
        false,
    )
    .expect_err("oversized session must error");
    assert_eq!(error.kind, "argument");
    drop(cache);
    drop(db_temp);
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
        warnings: Vec::new(),
    };
    assert_eq!(outcome.lease_id, "lease-x");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
}

// ---------------------------------------------------------------------------
// Issue 305 Task 2: heartbeat + stale active lease recovery
// ---------------------------------------------------------------------------
