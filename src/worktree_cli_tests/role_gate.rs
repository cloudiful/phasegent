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
