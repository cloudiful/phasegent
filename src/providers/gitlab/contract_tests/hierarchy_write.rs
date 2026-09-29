//! GitLab Work Item typed parent-write contracts (issue 641 P3).
//!
//! Verifies parent updates via `workItemUpdate`
//! (`hierarchyWidget.parentId`) for Epic-to-Issue and Issue-to-Task only,
//! with unsupported pairs failing as `not_supported` before any network and
//! no REST relation fallback.

use super::hierarchy::update_ok;
use super::support::{MockResponse, TEST_TOKEN, one, sequence, zero_request};
use crate::providers::hierarchy::{WorkItemKind, WorkItemRef};

fn issue_page() -> String {
    serde_json::json!({
        "data": {"workItem": {
            "id": "gid://gitlab/WorkItem/101",
            "workItemType": {"name": "Issue"},
            "namespace": {"fullPath": "acme/app"},
            "widgets": [{"__typename": "WorkItemWidgetHierarchy", "parent": null,
                "children": {"nodes": [], "pageInfo": {"hasNextPage": false}}}]}}})
    .to_string()
}

fn epic_page() -> String {
    serde_json::json!({
        "data": {"workItem": {
            "id": "gid://gitlab/WorkItem/100",
            "workItemType": {"name": "Epic"},
            "namespace": {"fullPath": "acme"},
            "widgets": [{"__typename": "WorkItemWidgetHierarchy", "parent": null,
                "children": {"nodes": [], "pageInfo": {"hasNextPage": false}}}]}}})
    .to_string()
}

#[test]
fn set_parent_epic_to_issue_posts_hierarchy_mutation() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(update_ok(101))]);
    let provider = super::support::provider(base);
    let child = WorkItemRef::gitlab(Some("42".to_owned()), 101, WorkItemKind::GitLabIssue);
    let parent = WorkItemRef::gitlab(Some("42".to_owned()), 100, WorkItemKind::GitLabEpic);
    provider.set_hierarchy_parent(&child, &parent).unwrap();
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    super::support::assert_request(&requests[0], "POST", "/api/graphql", Some("workItemUpdate"));
    assert!(requests[0].contains("hierarchyWidget"), "widget missing");
    assert!(
        requests[0].contains("gid://gitlab/WorkItem/101"),
        "child GID missing"
    );
    assert!(
        requests[0].contains("gid://gitlab/WorkItem/100"),
        "parent GID missing"
    );
    assert!(!requests[0].contains("/links"), "must not use relations");
    server.join().unwrap();
}

#[test]
fn set_parent_issue_to_task_succeeds() {
    let (result, _) = one(MockResponse::ok(update_ok(201)), |provider| {
        let child = WorkItemRef::gitlab(Some("42".to_owned()), 201, WorkItemKind::GitLabTask);
        let parent = WorkItemRef::gitlab(Some("42".to_owned()), 101, WorkItemKind::GitLabIssue);
        provider.set_hierarchy_parent(&child, &parent)
    });
    result.unwrap();
}

#[test]
fn set_parent_rejects_unsupported_pairs_without_network() {
    let epic = WorkItemKind::GitLabEpic;
    let issue = WorkItemKind::GitLabIssue;
    let task = WorkItemKind::GitLabTask;
    for (parent_kind, child_kind) in [
        (epic.clone(), task.clone()),
        (issue.clone(), issue.clone()),
        (task.clone(), issue.clone()),
        (epic.clone(), epic.clone()),
    ] {
        let child = WorkItemRef::gitlab(Some("42".to_owned()), 2, child_kind);
        let parent = WorkItemRef::gitlab(Some("42".to_owned()), 1, parent_kind);
        let error =
            zero_request(|provider| provider.set_hierarchy_parent(&child, &parent)).unwrap_err();
        assert_eq!(
            error.json()["kind"],
            "not_supported",
            "pair {parent:?}/{child:?}"
        );
    }
}

#[test]
fn set_parent_rejects_self_and_zero_without_network() {
    let child = WorkItemRef::gitlab(Some("42".to_owned()), 7, WorkItemKind::GitLabIssue);
    let parent = WorkItemRef::gitlab(Some("42".to_owned()), 6, WorkItemKind::GitLabEpic);
    let looped =
        zero_request(|provider| provider.set_hierarchy_parent(&child, &child)).unwrap_err();
    assert_eq!(looped.json()["kind"], "config");
    let zero = WorkItemRef::gitlab(Some("42".to_owned()), 0, WorkItemKind::GitLabIssue);
    let zero_error =
        zero_request(|provider| provider.set_hierarchy_parent(&zero, &parent)).unwrap_err();
    assert_eq!(zero_error.json()["kind"], "config");
}

#[test]
fn set_parent_mutation_errors_are_request_and_redacted() {
    let body = format!(
        r#"{{"data": {{"workItemUpdate": {{"errors": ["denied for {TEST_TOKEN}"], "workItem": null}}}}}}"#
    );
    let (result, _) = one(MockResponse::ok(body), |provider| {
        let child = WorkItemRef::gitlab(Some("42".to_owned()), 101, WorkItemKind::GitLabIssue);
        let parent = WorkItemRef::gitlab(Some("42".to_owned()), 100, WorkItemKind::GitLabEpic);
        provider.set_hierarchy_parent(&child, &parent)
    });
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "request");
    assert!(!error.json().to_string().contains(TEST_TOKEN));
}

#[test]
fn set_parent_by_id_resolves_native_kinds_before_mutation() {
    // Bare numeric IDs resolve through hierarchy reads first: the child
    // page yields the Issue kind, the parent page the Epic kind, and only
    // then does the widget mutation run. No REST relation is involved.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(issue_page()),
        MockResponse::ok(epic_page()),
        MockResponse::ok(update_ok(101)),
    ]);
    let provider = super::support::provider(base);
    provider.set_hierarchy_parent_by_id(101, 100).unwrap();
    let seen = requests.recv().unwrap();
    assert_eq!(seen.len(), 3);
    super::support::assert_request(&seen[0], "POST", "/api/graphql", Some("workItem"));
    super::support::assert_request(&seen[1], "POST", "/api/graphql", Some("workItem"));
    super::support::assert_request(&seen[2], "POST", "/api/graphql", Some("workItemUpdate"));
    assert!(seen[2].contains("hierarchyWidget"), "widget missing");
    assert!(
        seen[2].contains("gid://gitlab/WorkItem/101"),
        "child GID missing"
    );
    assert!(
        seen[2].contains("gid://gitlab/WorkItem/100"),
        "parent GID missing"
    );
    assert!(!seen[2].contains("/links"), "must not use relations");
    server.join().unwrap();
}

#[test]
fn set_parent_by_id_rejects_unsupported_pair_after_resolution() {
    // An Issue cannot parent an Epic: both reads happen, then the pair
    // fails as not_supported with no mutation issued.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(epic_page()),
        MockResponse::ok(issue_page()),
    ]);
    let provider = super::support::provider(base);
    let error = provider.set_hierarchy_parent_by_id(100, 101).unwrap_err();
    assert_eq!(error.json()["kind"], "not_supported");
    let seen = requests.recv().unwrap();
    assert_eq!(seen.len(), 2);
    server.join().unwrap();
}

#[test]
fn set_parent_by_id_rejects_zero_and_self_before_network() {
    for (child, parent) in [(0, 100), (101, 0), (101, 101)] {
        let error = zero_request(|provider| provider.set_hierarchy_parent_by_id(child, parent))
            .unwrap_err();
        assert_eq!(
            error.json()["kind"],
            "config",
            "child={child} parent={parent}"
        );
    }
}
