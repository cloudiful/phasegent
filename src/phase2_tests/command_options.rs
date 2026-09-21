use super::*;

#[test]
fn option_values_cannot_be_omitted() {
    for args in [
        vec!["--role", "orchestrator", "issue", "search", "--state"],
        vec![
            "--role",
            "orchestrator",
            "issue",
            "search",
            "--query",
            "--state",
            "all",
        ],
        vec!["--role", "orchestrator", "issue", "update", "1", "--body"],
        vec![
            "--role",
            "orchestrator",
            "issue",
            "create",
            "--title",
            "Title",
            "--body",
        ],
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(
            command::parse(&args).is_err(),
            "accepted missing value: {args:?}"
        );
    }
}

#[test]
fn issue_close_parses_worktree_session_option() {
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "close",
        "42",
        "--worktree-session",
        "alpha",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("close with --worktree-session parses");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Close {
            number,
            worktree_session,
        }) => {
            assert_eq!(number, 42);
            assert_eq!(worktree_session.as_deref(), Some("alpha"));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn issue_close_worktree_session_defaults_to_none() {
    // Legacy `issue close N` keeps parsing with no explicit session; the
    // CLI resolves PHASEGENT_SESSION_ID / legacy fallback at execution.
    let args = ["--role", "orchestrator", "issue", "close", "42"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("legacy close parses");
    match invocation.command {
        command::Command::Issue(command::IssueCommand::Close {
            number,
            worktree_session,
        }) => {
            assert_eq!(number, 42);
            assert_eq!(worktree_session, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn issue_close_rejects_blank_worktree_session() {
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "close",
        "42",
        "--worktree-session",
        "",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).unwrap_err();
    assert!(error.contains("session"), "unexpected error: {error}");
}

#[test]
fn issue_close_rejects_overlong_worktree_session() {
    let overlong = "s".repeat(129);
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "close",
        "42",
        "--worktree-session",
        overlong.as_str(),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).unwrap_err();
    assert!(
        error.contains("session") && error.contains("128"),
        "unexpected error: {error}"
    );
}

#[test]
fn issue_close_still_rejects_extra_positionals() {
    let args = ["--role", "orchestrator", "issue", "close", "42", "extra"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse(&args).is_err());
}

#[test]
fn issue_close_still_rejects_unknown_options() {
    let args = [
        "--role",
        "orchestrator",
        "issue",
        "close",
        "42",
        "--session",
        "alpha",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).unwrap_err();
    assert!(
        error.contains("unknown option"),
        "unexpected error: {error}"
    );
}

#[test]
fn issue_close_help_routes_to_command_topic() {
    let args = ["--role", "orchestrator", "issue", "close", "--help"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("issue close --help parses");
    match invocation.command {
        command::Command::Help(command::HelpTopic::IssueCommand(name)) => {
            assert_eq!(name, "close");
        }
        other => panic!("unexpected command {other:?}"),
    }
}

/// Issue 552 Phase 2 shipped `issue sync` without a help topic; the
/// routing table must resolve it to a command page instead of rejecting
/// the topic as unknown.
#[test]
fn issue_sync_help_routes_to_command_topic() {
    let args = ["--role", "orchestrator", "issue", "sync", "--help"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("issue sync --help parses");
    match invocation.command {
        command::Command::Help(command::HelpTopic::IssueCommand(name)) => {
            assert_eq!(name, "sync");
        }
        other => panic!("unexpected command {other:?}"),
    }
}
