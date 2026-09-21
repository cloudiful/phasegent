use super::support::*;
use super::*;

#[test]
fn executor_cannot_acquire() {
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Acquire {
            issue: 1,
            session: Some("s".to_owned()),
            base: None,
            format: "json".to_owned(),
            isolate: false,
            no_sync: false,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn reviewer_cannot_release() {
    let exit = execute_worktree(
        Some(Role::Reviewer),
        WorktreeCommand::Release {
            lease: "lease-1".to_owned(),
            retain: true,
            force: false,
            reason: None,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn executor_cannot_prune() {
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Prune {
            repo: None,
            stale_days: 14,
            release_stale: false,
            remove: false,
            reason: None,
            no_sync: false,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn tester_cannot_status() {
    // Use a synthetic `status` invocation that does not touch the
    // database: the role gate fires first.
    let exit = execute_worktree(Some(Role::Tester), WorktreeCommand::Status { issue: 1 });
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn tester_cannot_list() {
    let exit = execute_worktree(
        Some(Role::Tester),
        WorktreeCommand::List {
            repo: None,
            no_sync: false,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn admin_cannot_acquire() {
    let exit = execute_worktree(
        Some(Role::Admin),
        WorktreeCommand::Acquire {
            issue: 1,
            session: Some("s".to_owned()),
            base: None,
            format: "json".to_owned(),
            isolate: false,
            no_sync: false,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn orchestrator_status_passes_role_gate_and_queries_storage() {
    // The role gate is the first check; the storage call only fires
    // when the role is allowed. We use a temp DB so the operator's
    // real database is never touched.
    let _lock = lock_workflow_tests();
    let (_temp, _storage, _env) = open_temp_db("status-role");
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Status { issue: 1 },
    );
    assert_eq!(exit, 0, "orchestrator status must pass the role gate");
}

#[test]
fn executor_status_passes_role_gate() {
    let _lock = lock_workflow_tests();
    let (_temp, _storage, _env) = open_temp_db("status-executor");
    let exit = execute_worktree(Some(Role::Executor), WorktreeCommand::Status { issue: 1 });
    assert_eq!(exit, 0, "executor status must pass the role gate");
}

#[test]
fn reviewer_list_passes_role_gate() {
    let _lock = lock_workflow_tests();
    let (_temp, _storage, _env) = open_temp_db("list-reviewer");
    let exit = execute_worktree(
        Some(Role::Reviewer),
        WorktreeCommand::List {
            repo: None,
            no_sync: false,
        },
    );
    // List requires a real git repo; the test path lives under
    // /tmp so `repo_identity` will return an error and the
    // executor returns the structured error. The important
    // assertion is the exit code is NOT 3, i.e. the role gate
    // let the call through.
    assert_ne!(exit, 3, "reviewer list must pass the role gate");
}

// ---------------------------------------------------------------------------
// Prune pass: dry-run classification (no real git)
// ---------------------------------------------------------------------------

#[test]
fn executor_cannot_heartbeat() {
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Heartbeat {
            lease: "lease-1".to_owned(),
            session: Some("s".to_owned()),
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn reviewer_cannot_prune() {
    let exit = execute_worktree(
        Some(Role::Reviewer),
        WorktreeCommand::Prune {
            repo: None,
            stale_days: 14,
            release_stale: false,
            remove: false,
            reason: None,
            no_sync: false,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}
