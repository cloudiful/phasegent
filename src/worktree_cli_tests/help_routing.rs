use super::support::*;
use super::*;

#[test]
fn unknown_subcommand_is_rejected() {
    let err =
        crate::command::parse_with_role_env(&strings(["worktree", "fly"]), Some("orchestrator"))
            .unwrap_err();
    assert!(err.contains("fly"), "expected 'fly' in error, got {err}");
}

#[test]
fn top_level_help_routes_to_worktree() {
    let invocation =
        crate::command::parse_with_role_env(&strings(["worktree", "--help"]), Some("orchestrator"))
            .unwrap();
    match invocation.command {
        Command::Help(crate::command::HelpTopic::Worktree) => {}
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn subcommand_help_routes_to_command_topic() {
    for command in ["acquire", "status", "list", "probe", "prune"] {
        for args in [
            vec!["worktree", command, "--help"],
            vec!["--help", "worktree", command],
        ] {
            let argv: Vec<String> = args.iter().map(|value| (*value).to_owned()).collect();
            let invocation =
                crate::command::parse_with_role_env(&argv, Some("orchestrator")).unwrap();
            match invocation.command {
                Command::Help(crate::command::HelpTopic::WorktreeCommand(value)) => {
                    assert_eq!(value, command, "unexpected topic for {args:?}");
                }
                other => panic!("unexpected command {other:?} for {args:?}"),
            }
        }
    }
}

#[test]
fn removed_lease_subcommands_are_rejected_without_help() {
    for command in ["release", "heartbeat", "release-stale"] {
        let err = crate::command::parse_with_role_env(
            &strings(["worktree", command]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(
            err.contains("unknown worktree command"),
            "{command} must be removed, got {err}"
        );
    }
}
