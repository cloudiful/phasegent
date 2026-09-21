use super::support::*;
use super::*;

#[test]
fn parse_status_requires_issue() {
    let err =
        crate::command::parse(&strings(["--role", "executor", "worktree", "status"])).unwrap_err();
    assert!(err.contains("--issue"), "expected --issue error, got {err}");
}

#[test]
fn parse_list_repo_optional() {
    let invocation =
        crate::command::parse(&strings(["--role", "executor", "worktree", "list"])).unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::List { repo, .. }) => assert!(repo.is_none()),
        other => panic!("unexpected command {other:?}"),
    }
}
