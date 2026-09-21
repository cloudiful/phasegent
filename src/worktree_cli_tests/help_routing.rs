use super::support::*;
use super::*;

#[test]
fn unknown_subcommand_is_rejected() {
    let err =
        crate::command::parse(&strings(["--role", "orchestrator", "worktree", "fly"])).unwrap_err();
    assert!(err.contains("fly"), "expected 'fly' in error, got {err}");
}

#[test]
fn top_level_help_routes_to_worktree() {
    let invocation =
        crate::command::parse(&strings(["--role", "orchestrator", "worktree", "--help"])).unwrap();
    match invocation.command {
        Command::Help(crate::command::HelpTopic::Worktree) => {}
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn subcommand_help_routes_to_command_topic() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "acquire",
        "--help",
    ]))
    .unwrap();
    match invocation.command {
        Command::Help(crate::command::HelpTopic::WorktreeCommand(command)) => {
            assert_eq!(command, "acquire");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn root_help_topic_routes_heartbeat_and_prune() {
    for command in ["heartbeat", "prune"] {
        let invocation = crate::command::parse(&strings([
            "--role",
            "orchestrator",
            "--help",
            "worktree",
            command,
        ]))
        .unwrap();
        match invocation.command {
            Command::Help(crate::command::HelpTopic::WorktreeCommand(value)) => {
                assert_eq!(value, command);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Policy gates (command-level role checks)
// ---------------------------------------------------------------------------

#[test]
fn heartbeat_and_prune_help_route_to_command_topics() {
    for command in ["heartbeat", "prune"] {
        let invocation = crate::command::parse(&strings([
            "--role",
            "orchestrator",
            "worktree",
            command,
            "--help",
        ]))
        .unwrap();
        match invocation.command {
            Command::Help(crate::command::HelpTopic::WorktreeCommand(value)) => {
                assert_eq!(value, command);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
}
