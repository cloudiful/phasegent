use super::support::*;
use super::*;

#[test]
fn parse_acquire_minimal() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "acquire", "--issue", "1"]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Acquire {
            issue,
            session,
            base,
            format,
            isolate,
            no_sync: _,
        }) => {
            assert_eq!(issue, 1);
            assert_eq!(session, None);
            assert_eq!(base, None);
            assert_eq!(format, "json");
            assert!(!isolate, "--isolate must default off");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_acquire_isolate_flag_round_trips() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "acquire", "--issue", "247", "--isolate"]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Acquire { isolate, .. }) => {
            assert!(isolate, "--isolate must round-trip to the command");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_acquire_rejects_missing_issue() {
    let err = crate::command::parse_with_role_env(
        &strings(["worktree", "acquire"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(err.contains("--issue"), "expected --issue error, got {err}");
}

#[test]
fn parse_acquire_with_session_and_base() {
    let invocation = crate::command::parse_with_role_env(
        &strings([
            "worktree",
            "acquire",
            "--issue",
            "42",
            "--session",
            "alpha",
            "--base",
            "main",
        ]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Acquire {
            issue,
            session,
            base,
            format,
            isolate,
            no_sync: _,
        }) => {
            assert_eq!(issue, 42);
            assert_eq!(session.as_deref(), Some("alpha"));
            assert_eq!(base.as_deref(), Some("main"));
            assert_eq!(format, "json");
            assert!(!isolate);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn parse_acquire_rejects_non_json_format() {
    let err = crate::command::parse_with_role_env(
        &strings(["worktree", "acquire", "--issue", "1", "--format", "yaml"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(
        err.contains("--format"),
        "expected --format error, got {err}"
    );
}

#[test]
fn parse_acquire_rejects_non_numeric_issue() {
    let err = crate::command::parse_with_role_env(
        &strings(["worktree", "acquire", "--issue", "abc"]),
        Some("orchestrator"),
    )
    .unwrap_err();
    assert!(err.contains("--issue"), "expected --issue error, got {err}");
}

#[test]
fn parse_acquire_rejects_an_explicitly_empty_base() {
    // An explicit empty --base must not silently fall through to the
    // omitted-option decision table (reviewer note 8446): presence is
    // preserved and rejected.
    for args in [
        vec!["worktree", "acquire", "--issue", "595", "--base="],
        vec!["worktree", "acquire", "--issue", "595", "--base", ""],
        vec!["worktree", "acquire", "--issue", "595", "--base", "   "],
    ] {
        let argv: Vec<String> = args.iter().map(|value| (*value).to_owned()).collect();
        let err = crate::command::parse_with_role_env(&argv, Some("orchestrator")).unwrap_err();
        assert!(
            err.contains("--base") && err.contains("non-empty"),
            "an empty --base must be a structured parser error for {args:?}, got {err}"
        );
    }
}

#[test]
fn parse_acquire_still_omits_an_absent_base_and_trims_a_padded_one() {
    let invocation = crate::command::parse_with_role_env(
        &strings(["worktree", "acquire", "--issue", "595"]),
        Some("orchestrator"),
    )
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Acquire { base, .. }) => assert_eq!(base, None),
        other => panic!("unexpected command {other:?}"),
    }

    let padded = crate::command::parse_with_role_env(
        &strings([
            "worktree", "acquire", "--issue", "595", "--base", "  main  ",
        ]),
        Some("orchestrator"),
    )
    .unwrap();
    match padded.command {
        Command::Worktree(WorktreeCommand::Acquire { base, .. }) => {
            assert_eq!(base.as_deref(), Some("main"));
        }
        other => panic!("unexpected command {other:?}"),
    }
}
