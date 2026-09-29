//! Redmine native hierarchy wire contracts (issue 641 P2).
//!
//! Verifies `GET /issues/:id.json?include=children` maps native `parent` /
//! `children` fields into the provider-neutral [`HierarchyNode`] without
//! touching `relations`. All cases run against the local mock server.

use super::support;
use super::support::{MockResponse, one, sequence};
use crate::providers::hierarchy::WorkItemKind;

fn hierarchy_response(id: u64, parent: Option<u64>, children: &[u64]) -> String {
    let parent_value = parent.map_or(
        serde_json::Value::Null,
        |pid| serde_json::json!({"id": pid, "subject": "Parent"}),
    );
    serde_json::json!({
        "issue": {
            "id": id,
            "subject": "Subject",
            "description": "Body",
            "status": {"name": "New", "is_closed": false},
            "parent": parent_value,
            "children": children.iter().map(|cid| serde_json::json!({"id": cid, "subject": "Child"})).collect::<Vec<_>>(),
        }
    })
    .to_string()
}

#[test]
fn hierarchy_reports_native_parent_and_children() {
    let (result, request) = one(
        MockResponse::ok(hierarchy_response(641, Some(640), &[642, 643])),
        |redmine| redmine.get_hierarchy(641),
    );
    let node = result.unwrap();
    assert_eq!(node.item.id, 641);
    assert_eq!(node.parent_id(), Some(640));
    assert_eq!(node.children_ids(), vec![642, 643]);
    assert_eq!(node.item.kind, WorkItemKind::RedmineIssue);
    for item in std::iter::once(&node.item)
        .chain(node.parent.as_ref())
        .chain(node.children.iter())
    {
        assert_eq!(item.provider, "redmine");
        assert_eq!(item.kind, WorkItemKind::RedmineIssue);
    }
    support::assert_request(&request, "GET", "/issues/641.json?include=children", None);
}

#[test]
fn hierarchy_without_parent_returns_empty_children() {
    let (result, request) = one(
        MockResponse::ok(hierarchy_response(640, None, &[])),
        |redmine| redmine.get_hierarchy(640),
    );
    let node = result.unwrap();
    assert!(node.parent.is_none());
    assert!(node.children.is_empty());
    support::assert_request(&request, "GET", "/issues/640.json?include=children", None);
}

#[test]
fn hierarchy_tolerates_legacy_payload_without_native_keys() {
    let (result, request) = one(
        MockResponse::ok(super::support::issue_response(
            17,
            "Subject",
            "Body",
            false,
            &[],
        )),
        |redmine| redmine.get_hierarchy(17),
    );
    let node = result.unwrap();
    assert!(node.parent.is_none());
    assert!(node.children.is_empty());
    support::assert_request(&request, "GET", "/issues/17.json?include=children", None);
}

#[test]
fn hierarchy_never_reads_relations() {
    let body = serde_json::json!({
        "issue": {
            "id": 641,
            "subject": "Subject",
            "description": "Body",
            "status": {"name": "New", "is_closed": false},
            "parent": {"id": 640},
            "children": [{"id": 642}],
            "relations": [{"id": 9, "relation_type": "relates", "issue_id": 641, "issue_to_id": 640}],
        }
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |redmine| redmine.get_hierarchy(641));
    let node = result.unwrap();
    assert_eq!(node.parent_id(), Some(640));
    assert_eq!(node.children_ids(), vec![642]);
    assert!(
        !request.contains("relations"),
        "hierarchy read must not request relations: {request}"
    );
    support::assert_request(&request, "GET", "/issues/641.json?include=children", None);
}

#[test]
fn dispatcher_routes_hierarchy_to_redmine_and_rejects_gitlab() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(hierarchy_response(
        641,
        Some(640),
        &[642],
    ))]);
    let redmine = support::provider(base);
    let dispatcher = crate::providers::ProviderDispatcher::Redmine(redmine);
    let node = dispatcher.get_hierarchy(641).unwrap();
    assert_eq!(node.parent_id(), Some(640));
    let seen = requests.recv().unwrap();
    support::assert_request(&seen[0], "GET", "/issues/641.json?include=children", None);
    server.join().unwrap();

    // GitLab Work Item mapping lands in P3; P2 returns `not_supported`
    // without any network. Local/Forgejo stay on their existing arms
    // (owned by #642 / future work) and are not constructed here so this
    // contract never touches credentials or SQLite.
    let gitlab = crate::providers::ProviderDispatcher::Gitlab(
        crate::providers::gitlab::GitlabProvider::new(
            crate::providers::GitlabConfig::new("https://gitlab.example/api/v4", 42),
            "test-token".to_owned(),
        )
        .unwrap(),
    );
    let error = gitlab.get_hierarchy(1).unwrap_err();
    assert_eq!(error.json()["kind"], "not_supported");
}
