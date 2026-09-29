//! Redmine native hierarchy write contracts (issue 641 P4a).
//!
//! Verifies parent assignment and clearing through the native
//! `parent_issue_id` field (`PUT /issues/:child.json`), with an explicit
//! null clearing the parent. Split from the read contracts so neither file
//! exceeds the size budget. All cases run against the local mock server
//! and never touch relations.

use super::support;
use super::support::{MockResponse, one, sequence};

#[test]
fn set_parent_puts_native_parent_issue_id() {
    let (result, request) = one(MockResponse::status(204, String::new()), |redmine| {
        redmine.set_hierarchy_parent(642, 640)
    });
    result.unwrap();
    support::assert_request(
        &request,
        "PUT",
        "/issues/642.json",
        Some(r#""parent_issue_id":640"#),
    );
    assert!(
        !request.contains("relations"),
        "hierarchy write must not touch relations: {request}"
    );
}

#[test]
fn unset_parent_puts_explicit_null_parent_issue_id() {
    let (result, request) = one(MockResponse::status(204, String::new()), |redmine| {
        redmine.unset_hierarchy_parent(642)
    });
    result.unwrap();
    support::assert_request(
        &request,
        "PUT",
        "/issues/642.json",
        Some(r#""parent_issue_id":null"#),
    );
    assert!(
        !request.contains("relations"),
        "hierarchy write must not touch relations: {request}"
    );
}

#[test]
fn set_parent_rejects_zero_and_self_before_network() {
    // A closed-port base proves no request leaves the helper: validation
    // runs before any network access.
    let redmine = crate::providers::RedmineProvider::new(
        crate::providers::RedmineConfig::new("http://127.0.0.1:1", "42", 37),
        "test-key".to_owned(),
    )
    .unwrap();
    for (child, parent) in [(0, 640), (642, 0), (641, 641)] {
        let error = redmine.set_hierarchy_parent(child, parent).unwrap_err();
        assert_eq!(
            error.json()["kind"],
            "config",
            "child={child} parent={parent}"
        );
    }
    let error = redmine.unset_hierarchy_parent(0).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn dispatcher_routes_hierarchy_writes_to_redmine() {
    let (base, requests, server) = sequence(vec![
        MockResponse::status(204, String::new()),
        MockResponse::status(204, String::new()),
    ]);
    let redmine = support::provider(base);
    let dispatcher = crate::providers::ProviderDispatcher::Redmine(redmine);
    dispatcher.set_hierarchy_parent_by_id(642, 640).unwrap();
    dispatcher.unset_hierarchy_parent_by_id(642).unwrap();
    let seen = requests.recv().unwrap();
    assert_eq!(seen.len(), 2);
    support::assert_request(
        &seen[0],
        "PUT",
        "/issues/642.json",
        Some(r#""parent_issue_id":640"#),
    );
    support::assert_request(
        &seen[1],
        "PUT",
        "/issues/642.json",
        Some(r#""parent_issue_id":null"#),
    );
    server.join().unwrap();
}
