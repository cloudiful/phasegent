use super::support::*;
use super::*;

#[test]
fn issue_create_and_update_accept_optional_tracker_selection() {
    let create = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title",
        "Plan",
        "--tracker",
        "Bug",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&create).unwrap().command {
        command::Command::Issue(command::IssueCommand::Create {
            title,
            body,
            tracker,
            planning,
            ..
        }) => {
            assert_eq!(title, "Plan");
            assert_eq!(body, "");
            assert_eq!(tracker.as_deref(), Some("Bug"));
            assert!(planning.is_empty());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let update = [
        "--role",
        "orchestrator",
        "issue",
        "update",
        "9",
        "--body",
        "Updated",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&update).unwrap().command {
        command::Command::Issue(command::IssueCommand::Update {
            number,
            body,
            tracker,
            planning,
            ..
        }) => {
            assert_eq!(number, 9);
            assert_eq!(body, "Updated");
            assert!(tracker.is_none());
            assert!(planning.is_empty());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let unknown_tracker_option = [
        "--role",
        "orchestrator",
        "issue",
        "get",
        "9",
        "--tracker",
        "Bug",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&unknown_tracker_option).is_err());
}

#[test]
fn comment_get_uses_the_requested_issue_scope() {
    let (base, requests, server) = mock_server_with_headers(
        r#"[{"id":42,"body":"<!-- marker --> comment","html_url":"https://forgejo.example/comment/42"}]"#,
        &["X-Total-Count: 1"],
    );
    let provider = ForgejoProvider::new(
        ForgejoConfig::new(base, "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    let comment = provider.get_comment(7, 42).unwrap();
    assert_eq!(comment.id, 42);
    assert_eq!(comment.marker.as_deref(), Some("<!-- marker -->"));
    let request = requests.recv().unwrap();
    assert!(request.starts_with("GET /api/v1/repos/owner/repo/issues/7/comments?"));
    server.join().unwrap();
}

#[test]
fn comment_get_does_not_return_an_id_missing_from_issue_comments() {
    let (base, requests, server) =
        mock_server_with_headers(r#"[{"id":99,"body":"other"}]"#, &["X-Total-Count: 1"]);
    let provider = ForgejoProvider::new(
        ForgejoConfig::new(base, "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    let error = provider.get_comment(7, 42).unwrap_err();
    assert_eq!(error.json()["kind"], "not_found");
    assert!(
        requests
            .recv()
            .unwrap()
            .starts_with("GET /api/v1/repos/owner/repo/issues/7/comments?")
    );
    server.join().unwrap();
}

#[test]
fn issue_planning_flags_parse_on_create_and_update() {
    let create = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title=Plan",
        "--parent-issue",
        "12",
        "--fixed-version=Sprint 1",
        "--start-date",
        "2026-08-01",
        "--due-date",
        "2026-08-31",
        "--estimated-hours",
        "3.5",
        "--done-ratio",
        "40",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&create).unwrap().command {
        command::Command::Issue(command::IssueCommand::Create { planning, .. }) => {
            assert_eq!(planning.parent_issue.as_deref(), Some("12"));
            assert_eq!(planning.fixed_version.as_deref(), Some("Sprint 1"));
            assert_eq!(planning.start_date.as_deref(), Some("2026-08-01"));
            assert_eq!(planning.due_date.as_deref(), Some("2026-08-31"));
            assert_eq!(planning.estimated_hours.as_deref(), Some("3.5"));
            assert_eq!(planning.done_ratio.as_deref(), Some("40"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let update = [
        "--role",
        "orchestrator",
        "issue",
        "update",
        "9",
        "--body",
        "Updated",
        "--fixed-version",
        "7",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse(&update).unwrap().command {
        command::Command::Issue(command::IssueCommand::Update { planning, .. }) => {
            assert_eq!(planning.fixed_version.as_deref(), Some("7"));
            assert!(planning.parent_issue.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Planning flags belong only to create/update; other issue
    // subcommands must keep rejecting them.
    let misplaced = [
        "--role",
        "orchestrator",
        "issue",
        "get",
        "9",
        "--done-ratio",
        "50",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&misplaced).is_err());

    // A planning option missing its value keeps the strict missing-value
    // detection.
    let missing_value = [
        "--role",
        "orchestrator",
        "issue",
        "create",
        "--title",
        "T",
        "--start-date",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&missing_value).is_err());
}

#[test]
fn version_list_parses_and_rejects_unexpected_arguments() {
    for role in ["admin", "orchestrator", "executor", "reviewer"] {
        let args = ["--role", role, "--provider", "redmine", "version", "list"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(matches!(
            command::parse(&args).unwrap().command,
            command::Command::VersionCommand(command::VersionCommand::List)
        ));
    }

    // Bare `version` prints help rather than erroring; only unknown
    // subcommands and extra arguments are rejected.
    let args = vec![
        "--role",
        "executor",
        "--provider",
        "redmine",
        "version",
        "reorder",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&args).is_err());

    let args = vec![
        "--role",
        "executor",
        "--provider",
        "redmine",
        "version",
        "list",
        "extra",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert!(command::parse(&args).is_err());
}
