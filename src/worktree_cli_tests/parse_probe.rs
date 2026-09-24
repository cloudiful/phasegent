use super::support::*;
use super::*;

#[test]
fn parse_probe_defaults_to_the_current_checkout() {
    let invocation =
        crate::command::parse_with_role_env(&strings(["worktree", "probe"]), Some("executor"))
            .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Probe {
            path,
            issue,
            session,
        }) => {
            assert_eq!(path, None);
            assert_eq!(issue, None);
            assert_eq!(session, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_probe_path_selector_round_trips() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "probe", "--path", "/tmp/checkout"]),
        Some("executor"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Probe {
            path,
            issue,
            session,
        }) => {
            assert_eq!(path.as_deref(), Some("/tmp/checkout"));
            assert_eq!(issue, None);
            assert_eq!(session, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_probe_issue_session_selector_round_trips() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "probe", "--issue", "595", "--session", "alpha"]),
        Some("executor"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Probe {
            path,
            issue,
            session,
        }) => {
            assert_eq!(path, None);
            assert_eq!(issue, Some(595));
            assert_eq!(session.as_deref(), Some("alpha"));
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_probe_rejects_path_with_issue() {
    let err = crate::command::parse_with_role_env(
        &strings(["worktree", "probe", "--path", "/tmp/x", "--issue", "1"]),
        Some("executor"),
    )
    .unwrap_err();
    assert!(
        err.contains("mutually exclusive"),
        "expected a mutual-exclusion error, got {err}"
    );
}

#[test]
fn parse_probe_rejects_session_without_issue() {
    let err = crate::command::parse_with_role_env(
        &strings(["worktree", "probe", "--session", "alpha"]),
        Some("executor"),
    )
    .unwrap_err();
    assert!(
        err.contains("--session requires --issue"),
        "expected --session to require --issue, got {err}"
    );
}

#[test]
fn parse_probe_rejects_zero_and_non_numeric_issue() {
    for value in ["0", "abc"] {
        let err = crate::command::parse_with_role_env(
            &strings(["worktree", "probe", "--issue", value]),
            Some("executor"),
        )
        .unwrap_err();
        assert!(err.contains("--issue"), "expected --issue error, got {err}");
    }
}

#[test]
fn parse_probe_rejects_unknown_option() {
    let err = crate::command::parse_with_role_env(
        &strings(["worktree", "probe", "--repo", "/tmp/x"]),
        Some("executor"),
    )
    .unwrap_err();
    assert!(err.contains("--repo"), "expected unknown option, got {err}");
}

#[test]
fn parse_probe_rejects_an_explicitly_empty_path() {
    // An explicit empty --path must not silently fall back to the current
    // checkout (reviewer note 8446): presence is preserved and rejected.
    for args in [
        vec!["worktree", "probe", "--path="],
        vec!["worktree", "probe", "--path", ""],
        vec!["worktree", "probe", "--path", "   "],
    ] {
        let argv: Vec<String> = args.iter().map(|value| (*value).to_owned()).collect();
        let err = crate::command::parse_with_role_env(&argv, Some("executor")).unwrap_err();
        assert!(
            err.contains("--path") && err.contains("non-empty"),
            "an empty --path must be a structured parser error for {args:?}, got {err}"
        );
    }
}

#[test]
fn parse_probe_still_omits_an_absent_path() {
    // Omitted stays None: only an explicit value is rejected.
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "probe", "--issue", "595"]),
        Some("executor"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Probe { path, .. }) => assert_eq!(path, None),
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn probe_help_routes_to_the_command_topic() {
    for args in [
        ["worktree", "probe", "--help"],
        ["--help", "worktree", "probe"],
    ] {
        let invocation =
            crate::command::parse_with_role_env(&strings(args), Some("executor")).unwrap();
        match invocation.command {
            Command::Help(crate::command::HelpTopic::WorktreeCommand(command)) => {
                assert_eq!(command, "probe");
            }
            other => panic!("unexpected command {other:?} for {args:?}"),
        }
    }
}
