//! GitLab Work Item parent-clear contracts (issue 641 P4a).
//!
//! Verifies clearing a parent via `workItemUpdate` with an explicit null
//! `parentId` in the hierarchy widget: the mutation carries only the child
//! GID, mutation errors stay structured and token-redacted, and no REST
//! relation fallback is involved.

use super::hierarchy::update_ok;
use super::support::{MockResponse, TEST_TOKEN, one, sequence, zero_request};

#[test]
fn unset_parent_posts_null_parent_mutation() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(update_ok(101))]);
    let provider = super::support::provider(base);
    provider.unset_hierarchy_parent_by_id(101).unwrap();
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    super::support::assert_request(&requests[0], "POST", "/api/graphql", Some("workItemUpdate"));
    assert!(requests[0].contains("hierarchyWidget"), "widget missing");
    assert!(
        requests[0].contains("gid://gitlab/WorkItem/101"),
        "child GID missing"
    );
    assert!(
        requests[0].contains("\"parentId\":null"),
        "unset must send an explicit null parent: {}",
        requests[0]
    );
    assert!(!requests[0].contains("/links"), "must not use relations");
    server.join().unwrap();
}

#[test]
fn unset_parent_rejects_zero_id_before_network() {
    let error = zero_request(|provider| provider.unset_hierarchy_parent_by_id(0)).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn unset_parent_mutation_errors_are_request_and_redacted() {
    let body = format!(
        r#"{{"data": {{"workItemUpdate": {{"errors": ["denied for {TEST_TOKEN}"], "workItem": null}}}}}}"#
    );
    let (result, _) = one(MockResponse::ok(body), |provider| {
        provider.unset_hierarchy_parent_by_id(101)
    });
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "request");
    assert!(!error.json().to_string().contains(TEST_TOKEN));
}

#[test]
fn dispatcher_routes_hierarchy_unset_to_work_items() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(update_ok(101))]);
    let dispatcher = super::support::dispatcher(base);
    dispatcher.unset_hierarchy_parent_by_id(101).unwrap();
    let request = requests.recv().unwrap().remove(0);
    assert!(request.starts_with("POST /api/graphql"), "{request}");
    assert!(request.contains("workItemUpdate"), "{request}");
    server.join().unwrap();
}
