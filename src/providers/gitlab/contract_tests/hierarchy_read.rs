//! GitLab Work Item hierarchy read contracts (issue 641 P3).
//!
//! Verifies exact kind/ref mapping (Epic/Issue/Task never collapse) and
//! hierarchy reads via `workItem(id:)` plus the `widgets` hierarchy widget,
//! with per-item `namespace.fullPath` scope, explicit truncation reporting
//! from `pageInfo.hasNextPage`, structured capability errors, and no REST
//! relation fallback.

use super::hierarchy::{epic_with_issue_children, issue_with_epic_parent_and_task_child};
use super::support::{MockResponse, TEST_TOKEN, one, sequence, zero_request};
use crate::providers::hierarchy::WorkItemKind;

fn scoped_epic(has_next_page: bool) -> String {
    serde_json::json!({
        "data": {"workItem": {
            "id": "gid://gitlab/WorkItem/100",
            "workItemType": {"name": "Epic"},
            "namespace": {"fullPath": "acme"},
            "widgets": [{"__typename": "WorkItemWidgetDescription"},
                {"__typename": "WorkItemWidgetHierarchy", "parent": null,
                 "children": {"nodes": [
                     {"id": "gid://gitlab/WorkItem/101", "workItemType": {"name": "Issue"}, "namespace": {"fullPath": "acme/app"}},
                     {"id": "gid://gitlab/WorkItem/102", "workItemType": {"name": "Issue"}}],
                  "pageInfo": {"hasNextPage": has_next_page}}}]}}}).to_string()
}

fn scoped_issue() -> String {
    serde_json::json!({
        "data": {"workItem": {
            "id": "gid://gitlab/WorkItem/101",
            "workItemType": {"name": "Issue"},
            "namespace": {"fullPath": "acme/app"},
            "widgets": [{"__typename": "WorkItemWidgetHierarchy",
                "parent": {"id": "gid://gitlab/WorkItem/100", "workItemType": {"name": "Epic"}, "namespace": {"fullPath": "acme"}},
                "children": {"nodes": [
                    {"id": "gid://gitlab/WorkItem/201", "workItemType": {"name": "Task"}, "namespace": {"fullPath": "acme/app"}}],
                 "pageInfo": {"hasNextPage": false}}}]}}}).to_string()
}

#[test]
fn hierarchy_reads_epic_with_issue_children() {
    let (result, request) = one(MockResponse::ok(scoped_epic(false)), |provider| {
        provider.get_hierarchy(100)
    });
    let node = result.unwrap();
    assert_eq!(node.item.id, 100);
    assert_eq!(node.item.kind, WorkItemKind::GitLabEpic);
    assert_eq!(node.item.project.as_deref(), Some("acme"));
    assert!(node.parent.is_none());
    assert_eq!(node.children_ids(), vec![101, 102]);
    assert_eq!(node.children[0].project.as_deref(), Some("acme/app"));
    assert_eq!(node.children[1].project.as_deref(), Some("acme"));
    super::support::assert_request(&request, "POST", "/api/graphql", Some("workItem"));
    for fragment in [
        "WorkItemWidgetHierarchy",
        "fullPath",
        "hasNextPage",
        "first: 50",
    ] {
        assert!(request.contains(fragment), "{fragment}: {request}");
    }
    assert!(!request.contains("/links"), "relations leak: {request}");
}

#[test]
fn hierarchy_reads_issue_parent_and_task_child() {
    let (result, _) = one(MockResponse::ok(scoped_issue()), |provider| {
        provider.get_hierarchy(101)
    });
    let node = result.unwrap();
    assert_eq!(node.item.kind, WorkItemKind::GitLabIssue);
    assert_eq!(node.item.project.as_deref(), Some("acme/app"));
    let parent = node.parent.as_ref().expect("epic parent");
    assert_eq!(parent.id, 100);
    assert_eq!(parent.kind, WorkItemKind::GitLabEpic);
    assert_eq!(parent.project.as_deref(), Some("acme"));
    assert_eq!(node.children_ids(), vec![201]);
    assert_eq!(node.children[0].kind, WorkItemKind::GitLabTask);
    assert_eq!(node.children[0].project.as_deref(), Some("acme/app"));
}

#[test]
fn hierarchy_kinds_never_collapse() {
    let (result, _) = one(MockResponse::ok(scoped_epic(false)), |provider| {
        provider.get_hierarchy(100)
    });
    let node = result.unwrap();
    assert_ne!(node.item.kind, node.children[0].kind);
    assert!(node.item.kind != WorkItemKind::GitLabIssue);
    assert!(node.item.kind != WorkItemKind::GitLabTask);
    assert!(node.item.kind != WorkItemKind::RedmineIssue);
}

#[test]
fn hierarchy_reports_truncation_from_page_info() {
    let (result, _) = one(MockResponse::ok(scoped_epic(true)), |provider| {
        provider.get_hierarchy_page(100)
    });
    let page = result.unwrap();
    assert!(page.children_truncated);
    assert_eq!(page.node.children_ids(), vec![101, 102]);
    let (result, _) = one(MockResponse::ok(scoped_epic(false)), |provider| {
        provider.get_hierarchy_page(100)
    });
    let page = result.unwrap();
    assert!(!page.children_truncated);
    assert_eq!(page.node.children_ids(), vec![101, 102]);
}

#[test]
fn hierarchy_missing_widget_is_not_supported() {
    let body = serde_json::json!({
        "data": {"workItem": {
            "id": "gid://gitlab/WorkItem/101",
            "workItemType": {"name": "Issue"},
            "namespace": {"fullPath": "acme/app"},
            "widgets": [{"__typename": "WorkItemWidgetDescription"}],
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
            "namespace": {"fullPath": "acme/app"},
            "widgets": [{"__typename": "WorkItemWidgetHierarchy", "parent": null,
                "children": {"nodes": [], "pageInfo": {"hasNextPage": false}}}],
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
    let (base, requests, server) = sequence(vec![MockResponse::ok(scoped_epic(false))]);
    let dispatcher = super::support::dispatcher(base);
    let node = dispatcher.get_hierarchy(100).unwrap();
    assert_eq!(node.item.kind, WorkItemKind::GitLabEpic);
    assert_eq!(node.item.project.as_deref(), Some("acme"));
    assert_eq!(node.children_ids(), vec![101, 102]);
    let request = requests.recv().unwrap().remove(0);
    assert!(request.starts_with("POST /api/graphql"), "{request}");
    server.join().unwrap();
}

#[test]
fn hierarchy_legacy_direct_field_shape_is_not_supported() {
    // The pre-fix guessed `hierarchy` field is not part of the real schema:
    // without a `widgets` hierarchy entry the instance is treated as lacking
    // hierarchy support instead of silently mapping the guessed shape.
    for legacy in [
        epic_with_issue_children(),
        issue_with_epic_parent_and_task_child(),
    ] {
        let (result, _) = one(MockResponse::ok(legacy), |provider| {
            provider.get_hierarchy(100)
        });
        assert_eq!(result.unwrap_err().json()["kind"], "not_supported");
    }
}
