use super::support::*;
use super::*;

fn acquire_command() -> WorktreeCommand {
    WorktreeCommand::Acquire {
        issue: 1,
        session: Some("s".to_owned()),
        base: None,
        format: "json".to_owned(),
        isolate: false,
        no_sync: true,
    }
}

fn prune_command() -> WorktreeCommand {
    WorktreeCommand::Prune {
        repo: None,
        remove: false,
        dry_run: true,
        format: "json".to_owned(),
        verbose: false,
        no_sync: true,
    }
}

#[test]
fn executor_cannot_acquire() {
    let exit = execute_worktree(Some(Role::Executor), acquire_command());
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn reviewer_cannot_acquire() {
    let exit = execute_worktree(Some(Role::Reviewer), acquire_command());
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn executor_cannot_prune() {
    let exit = execute_worktree(Some(Role::Executor), prune_command());
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn reviewer_cannot_prune() {
    let exit = execute_worktree(Some(Role::Reviewer), prune_command());
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn admin_cannot_status() {
    let exit = execute_worktree(
        Some(Role::Admin),
        WorktreeCommand::Status {
            issue: 1,
            page: None,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn reviewer_status_passes_role_gate() {
    let _lock = lock_workflow_tests();
    let (_temp, _env) = open_temp_db("status-reviewer");
    let exit = execute_worktree(
        Some(Role::Reviewer),
        WorktreeCommand::Status {
            issue: 1,
            page: None,
        },
    );
    assert_eq!(exit, 0, "reviewer status must pass the role gate");
}

#[test]
fn admin_cannot_list() {
    let exit = execute_worktree(
        Some(Role::Admin),
        WorktreeCommand::List {
            repo: None,
            no_sync: true,
            page: None,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn admin_cannot_probe() {
    let exit = execute_worktree(
        Some(Role::Admin),
        WorktreeCommand::Probe {
            path: None,
            issue: None,
            session: None,
        },
    );
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn admin_cannot_acquire() {
    let exit = execute_worktree(Some(Role::Admin), acquire_command());
    assert_eq!(exit, 3, "permission error must return exit code 3");
}

#[test]
fn orchestrator_status_passes_role_gate_and_queries_storage() {
    let _lock = lock_workflow_tests();
    let (_temp, _env) = open_temp_db("status-role");
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Status {
            issue: 1,
            page: None,
        },
    );
    assert_eq!(exit, 0, "orchestrator status must pass the role gate");
}

#[test]
fn executor_status_passes_role_gate() {
    let _lock = lock_workflow_tests();
    let (_temp, _env) = open_temp_db("status-executor");
    let exit = execute_worktree(
        Some(Role::Executor),
        WorktreeCommand::Status {
            issue: 1,
            page: None,
        },
    );
    assert_eq!(exit, 0, "executor status must pass the role gate");
}

#[test]
fn reviewer_list_passes_role_gate() {
    let _lock = lock_workflow_tests();
    let (_temp, _env) = open_temp_db("list-reviewer");
    let exit = execute_worktree(
        Some(Role::Reviewer),
        WorktreeCommand::List {
            repo: None,
            no_sync: true,
            page: None,
        },
    );
    assert_ne!(exit, 3, "reviewer list must pass the role gate");
}
