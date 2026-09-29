//! GitLab Work Item hierarchy read contracts (issue 641 P3).
//!
//! Verifies exact kind/ref mapping (Epic/Issue/Task never collapse) and
//! hierarchy reads via `workItem(id:)` plus its hierarchy widget, with
//! structured capability errors and no REST relation fallback.

use super::hierarchy::{epic_with_issue_children, issue_with_epic_parent_and_task_child};
use super::support::{MockResponse, TEST_TOKEN, one, sequence, zero_request};
use crate::providers::hierarchy::WorkItemKind;

#[test]
fn hierarchy_reads_epic_with_issue_children() {
    let (result, request) = one(MockResponse::ok(epic_with_issue_children()), |provider| {
        provider.get_hierarchy(100)
    });
    let node = result.unwrap();
    assert_eq!(node.item.id, 100);
    assert_eq!(node.item.kind, WorkItemKind::GitLabEpic);
    assert!(node.parent.is_none());
    assert_eq!(node.children_ids(), vec![101, 102]);
    for child in &node.children {
        assert_eq!(child.kind, WorkItemKind::GitLabIssue);
        assert_eq!(child.provider, "gitlab");
        assert_eq!(child.project.as_deref(), Some("42"));
    }
    assert_eq!(node.item.provider, "gitlab");
    assert_eq!(node.item.project.as_deref(), Some("42"));
    super::support::assert_request(&request, "POST", "/api/graphql", Some("workItem"));
    assert!(request.contains("hierarchy"), "widget missing: {request}");
    assert!(
        !request.contains("/links"),
        "hierarchy must never use REST relations: {request}"
    );
}

#[test]
fn hierarchy_reads_issue_parent_and_task_child() {
    let (result, _) = one(
        MockResponse::ok(issue_with_epic_parent_and_task_child()),
        |provider| provider.get_hierarchy(101),
    );
    let node = result.unwrap();
    assert_eq!(node.item.kind, WorkItemKind::GitLabIssue);
    let parent = node.parent.as_ref().expect("epic parent");
    assert_eq!(parent.id, 100);
    assert_eq!(parent.kind, WorkItemKind::GitLabEpic);
    assert_eq!(node.children_ids(), vec![201]);
    assert_eq!(node.children[0].kind, WorkItemKind::GitLabTask);
}

#[test]
fn hierarchy_kinds_never_collapse() {
    let (result, _) = one(MockResponse::ok(epic_with_issue_children()), |provider| {
        provider.get_hierarchy(100)
    });
    let node = result.unwrap();
    assert_ne!(node.item.kind, node.children[0].kind);
    assert!(node.item.kind != WorkItemKind::GitLabIssue);
    assert!(node.item.kind != WorkItemKind::GitLabTask);
    assert!(node.item.kind != WorkItemKind::RedmineIssue);
}

#[test]
fn hierarchy_missing_widget_is_not_supported() {
    let body = serde_json::json!({
        "data": {"workItem": {
            "id": "gid://gitlab/WorkItem/101",
            "workItemType": {"name": "Issue"},
            "hierarchy": null,
        }}
    })
    .to_string();
    let (result, _) = one(MockResponse::ok(body), |provider| {
        provider.get_hierarchy(101)
    });
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "not_supported");
}

#[test]
fn hierarchy_unknown_type_is_decode() {
    let body = serde_json::json!({
        "data": {"workItem": {
            "id": "gid://gitlab/WorkItem/101",
            "workItemType": {"name": "Objective"},
            "hierarchy": {"parent": null, "children": {"nodes": []}},
        }}
    })
    .to_string();
    let (result, _) = one(MockResponse::ok(body), |provider| {
        provider.get_hierarchy(101)
    });
    assert_eq!(result.unwrap_err().json()["kind"], "decode");
}

#[test]
fn hierarchy_missing_item_is_not_found() {
    let (result, _) = one(
        MockResponse::ok(r#"{"data": {"workItem": null}}"#.to_owned()),
        |provider| provider.get_hierarchy(999),
    );
    assert_eq!(result.unwrap_err().json()["kind"], "not_found");
}

#[test]
fn hierarchy_graphql_errors_are_request_and_redacted() {
    let body = format!(r#"{{"errors":[{{"message":"denied for {TEST_TOKEN}"}}], "data": null}}"#);
    let (result, _) = one(MockResponse::ok(body), |provider| {
        provider.get_hierarchy(101)
    });
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "request");
    assert!(!error.json().to_string().contains(TEST_TOKEN));
}

#[test]
fn hierarchy_rejects_zero_id_before_network() {
    let error = zero_request(|provider| provider.get_hierarchy(0)).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn dispatcher_routes_gitlab_hierarchy_to_work_items() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(epic_with_issue_children())]);
    let dispatcher = super::support::dispatcher(base);
    let node = dispatcher.get_hierarchy(100).unwrap();
    assert_eq!(node.item.kind, WorkItemKind::GitLabEpic);
    assert_eq!(node.children_ids(), vec![101, 102]);
    let requests = requests.recv().unwrap();
    assert!(
        requests[0].starts_with("POST /api/graphql"),
        "{}",
        requests[0]
    );
    server.join().unwrap();
}
