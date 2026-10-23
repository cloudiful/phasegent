use super::support::*;
use super::*;

#[test]
fn parse_status_requires_issue() {
    let err =
        crate::command::parse_with_role_env(&strings(["worktree", "status"]), Some("executor"))
            .unwrap_err();
    assert!(err.contains("--issue"), "expected --issue error, got {err}");
}

#[test]
fn explicit_pages_are_checked_and_timer_limit_only_uses_the_shared_contract() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "list", "--limit", "7"]),
        Some("executor"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::List {
            page: Some(page), ..
        }) => {
            assert_eq!(page.page(), 1);
            assert_eq!(page.limit(), 7);
            assert_eq!(page.offset(), 0);
        }
        other => panic!("{other:?}"),
    }
    for args in [
        strings(["timer", "list"]),
        strings(["timer", "list", "--limit", "7"]),
        strings(["timer", "list", "--page", "2"]),
    ] {
        let invocation = crate::command::parse_with_role_env(&args, Some("orchestrator")).unwrap();
        let limit_only = args.get(2).is_some_and(|value| value == "--limit");
        let expected_page = if args.len() == 2 || limit_only { 1 } else { 2 };
        let expected_limit = if limit_only { 7 } else { 50 };
        let expected_offset = if expected_page == 1 { 0 } else { 50 };
        match invocation.command {
            Command::Timer(crate::command::TimerCommand::List { page, limit, .. }) => {
                let page = page.expect("timer parser supplies a page request");
                assert_eq!(limit, expected_limit);
                assert_eq!(page.page(), expected_page);
                assert_eq!(page.limit(), expected_limit as usize);
                assert_eq!(page.offset(), expected_offset);
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(
        crate::command::parse_with_role_env(
            &strings(["timer", "list", "--limit", "1000"]),
            Some("orchestrator")
        )
        .is_err()
    );
    for args in [
        strings(["worktree", "list", "--page=0"]),
        strings(["worktree", "list", "--limit=101"]),
        strings(["worktree", "prune", "--page=1"]),
    ] {
        assert!(crate::command::parse_with_role_env(&args, Some("orchestrator")).is_err());
    }
}

#[test]
fn parse_list_repo_optional() {
    let invocation =
        crate::command::parse_with_role_env(&strings(["worktree", "list"]), Some("executor"))
            .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::List { repo, .. }) => assert!(repo.is_none()),
        other => panic!("unexpected command {other:?}"),
    }
}
