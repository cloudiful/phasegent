use super::support::*;
use super::*;
use crate::providers::{RedmineConfig, RedmineProvider};

#[test]
fn issue_create_and_update_accept_optional_tracker_selection() {
    let create = ["issue", "create", "--title", "Plan", "--tracker", "Bug"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match command::parse_with_role_env(&create, Some("orchestrator"))
        .unwrap()
        .command
    {
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

    let update = ["issue", "update", "9", "--body", "Updated"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match command::parse_with_role_env(&update, Some("orchestrator"))
        .unwrap()
        .command
    {
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

    let unknown_tracker_option = ["issue", "get", "9", "--tracker", "Bug"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse_with_role_env(&unknown_tracker_option, Some("orchestrator")).is_err());
}

#[test]
fn comment_get_uses_the_requested_issue_scope() {
    let (base, requests, server) = mock_server_with_headers(
        r#"{"issue":{"id":7,"subject":"Title","description":"","status":{"name":"New","is_closed":false},"journals":[{"id":42,"notes":"<!-- marker --> comment"}]}}"#,
        &[],
    );
    let provider = RedmineProvider::new(
        RedmineConfig::new(base, "tools-phasegent", 2),
        "token".to_owned(),
    )
    .unwrap();
    let comment = provider.get_comment(7, 42).unwrap();
    assert_eq!(comment.id, 42);
    assert_eq!(comment.marker.as_deref(), Some("<!-- marker -->"));
    let request = requests.recv().unwrap();
    assert!(request.starts_with("GET /api/v1/issues/7.json?include=journals"));
    server.join().unwrap();
}

#[test]
fn comment_get_does_not_return_an_id_missing_from_issue_comments() {
    let (base, requests, server) = mock_server_with_headers(
        r#"{"issue":{"id":7,"subject":"Title","description":"","status":{"name":"New","is_closed":false},"journals":[{"id":99,"notes":"other"}]}}"#,
        &[],
    );
    let provider = RedmineProvider::new(
        RedmineConfig::new(base, "tools-phasegent", 2),
        "token".to_owned(),
    )
    .unwrap();
    let error = provider.get_comment(7, 42).unwrap_err();
    assert_eq!(error.json()["kind"], "not_found");
    assert!(
        requests
            .recv()
            .unwrap()
            .starts_with("GET /api/v1/issues/7.json?include=journals")
    );
    server.join().unwrap();
}

#[test]
fn issue_planning_flags_parse_on_create_and_update() {
    let create = [
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
    match command::parse_with_role_env(&create, Some("orchestrator"))
        .unwrap()
        .command
    {
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
    match command::parse_with_role_env(&update, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Issue(command::IssueCommand::Update { planning, .. }) => {
            assert_eq!(planning.fixed_version.as_deref(), Some("7"));
            assert!(planning.parent_issue.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Planning flags belong only to create/update; other issue
    // subcommands must keep rejecting them.
    let misplaced = ["issue", "get", "9", "--done-ratio", "50"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse_with_role_env(&misplaced, Some("orchestrator")).is_err());

    // A planning option missing its value keeps the strict missing-value
    // detection.
    let missing_value = ["issue", "create", "--title", "T", "--start-date"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse_with_role_env(&missing_value, Some("orchestrator")).is_err());
}

#[test]
fn version_list_parses_and_rejects_unexpected_arguments() {
    for role in ["admin", "orchestrator", "executor", "reviewer"] {
        let args = ["--provider", "redmine", "version", "list"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(matches!(
            command::parse_with_role_env(&args, Some(role))
                .unwrap()
                .command,
            command::Command::VersionCommand(command::VersionCommand::List)
        ));
    }

    // Bare `version` prints help rather than erroring; only unknown
    // subcommands and extra arguments are rejected.
    let args = vec!["--provider", "redmine", "version", "reorder"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse_with_role_env(&args, Some("executor")).is_err());

    let args = vec!["--provider", "redmine", "version", "list", "extra"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse_with_role_env(&args, Some("executor")).is_err());
}

#[test]
fn redmine_native_subtask_skips_relates_auto_without_network() {
    // Issue 641 P2: a Redmine child created with native `--parent-issue`
    // must not attempt a `relates` edge (Redmine answers 422 for
    // parent/subtask pairs). The helper returns `Skipped` before any
    // list/create call, so a closed-port base proves no network happens.
    let redmine = crate::providers::RedmineProvider::new(
        crate::providers::RedmineConfig::new("http://127.0.0.1:1", "42", 37),
        "test-key".to_owned(),
    )
    .unwrap();
    let provider = crate::providers::ProviderDispatcher::Redmine(redmine);
    let outcome =
        crate::lifecycle_auto::auto_create_parent_child_relation(&provider, 641, Some(640));
    match &outcome {
        crate::lifecycle_auto::AutoRelationOutcome::Skipped { reason } => {
            assert!(
                reason.contains("native") || reason.contains("hierarchy"),
                "reason must name native hierarchy: {reason}"
            );
        }
        other => panic!("expected Skipped for Redmine native subtask, got {other:?}"),
    }
    assert!(outcome.warning().is_none());
}

#[test]
fn hierarchy_contract_stays_distinct_from_relations() {
    // Provider-neutral hierarchy never collapses into `relates`: Redmine
    // nesting is supported, self-parent is rejected, and Local has no
    // hierarchy surface in P2.
    use crate::providers::hierarchy::{
        HierarchyEdge, HierarchyNode, WorkItemKind, WorkItemRef, supported_parent_child,
    };
    assert!(supported_parent_child(
        &WorkItemKind::RedmineIssue,
        &WorkItemKind::RedmineIssue
    ));
    assert!(!supported_parent_child(
        &WorkItemKind::LocalIssue,
        &WorkItemKind::LocalIssue
    ));
    let parent = WorkItemRef::redmine(Some("42".to_owned()), 640);
    let child = WorkItemRef::redmine(Some("42".to_owned()), 641);
    let edge = HierarchyEdge {
        parent: parent.clone(),
        child: child.clone(),
    };
    assert!(edge.validate().is_ok());
    let looped = HierarchyEdge {
        parent: parent.clone(),
        child: parent.clone(),
    };
    assert!(looped.validate().is_err());
    let node = HierarchyNode {
        item: child,
        parent: Some(parent),
        children: Vec::new(),
    };
    assert_eq!(node.parent_id(), Some(640));
    assert!(node.children_ids().is_empty());
}
