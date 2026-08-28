#![allow(unused_imports)]
use super::support;
use super::support::{
    MockResponse, TEST_API_KEY, current_user_response, git_mirror_response, issue_collection,
    issue_response, membership_collection, membership_collection_page, mirror_env, one,
    project_collection, project_response, provider, role_collection, role_collection_page,
    sequence, strings, time_entry_activities, time_entry_collection, time_entry_response,
    user_from_response, version_collection, version_collection_page,
};
use crate::auth;
use crate::ci_model::CiRunsFilter;
use crate::command::{
    self, Command, IssueCommand, ProjectCommand, RelationCommand, StatusCommand, WorkflowCommand,
};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{Storage, TimerRun};
use crate::policy::{Capability, Role};
use crate::providers::redmine::model::{RedmineRelationType, RedmineTimeEntryActivity};
use crate::providers::{
    ProviderDispatcher, ProviderKind, RedmineConfig, RedmineIssueStatus, RedmineMetadataProvider,
    RedmineProvider,
};
use std::str::FromStr;
use std::{fs, time};

#[test]
fn get_issue_uses_redmine_json_journals_and_maps_summary() {
    let (result, request) = one(
        MockResponse::ok(issue_response(17, "Subject", "Description", false, &[])),
        |redmine| redmine.get_issue(17),
    );
    let issue = result.unwrap();
    assert_eq!(issue.number, 17);
    assert_eq!(issue.title, "Subject");
    assert_eq!(issue.body, "Description");
    assert_eq!(issue.state, "open");
    assert!(
        issue
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/issues/17"))
    );
    support::assert_request(&request, "GET", "/issues/17.json?include=journals", None);
}

#[test]
fn create_issue_wraps_project_and_fields_as_json() {
    let (result, request) = one(
        MockResponse::ok(issue_response(18, "Created", "Body", false, &[])),
        |redmine| redmine.create_issue("Created", "Body"),
    );
    assert_eq!(result.unwrap().number, 18);
    support::assert_request(&request, "POST", "/issues.json", None);
    assert!(request.contains("content-type: application/json"));
    assert!(
        request.contains(r#""issue":{"project_id":42,"subject":"Created","description":"Body"}"#)
    );
}

#[test]
fn update_body_uses_put_and_description_wrapper() {
    let (result, request) = one(
        MockResponse::ok(issue_response(19, "Title", "Updated", false, &[])),
        |redmine| redmine.update_body(19, "Updated"),
    );
    assert_eq!(result.unwrap().body, "Updated");
    support::assert_request(&request, "PUT", "/issues/19.json", None);
    assert!(request.contains(r#""issue":{"description":"Updated"}"#));
    assert!(!request.contains("status_id"));
}

#[test]
fn close_uses_the_configured_status_id() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        20,
        "Title",
        "Body",
        true,
        &[],
    ))]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    assert!(redmine.close_issue(20).is_ok());
    let request = requests.recv().unwrap().remove(0);
    support::assert_request(&request, "PUT", "/issues/20.json", None);
    assert!(request.contains(r#""issue":{"status_id":37}"#));
    assert!(!request.contains(r#""status_id":5"#));
    server.join().unwrap();
}

#[test]
fn update_body_with_tracker_keeps_single_put_shape() {
    let (result, request) = one(
        MockResponse::ok(issue_response(23, "Title", "Updated", false, &[])),
        |redmine| redmine.update_body_with_tracker(23, "Updated", 1),
    );
    assert_eq!(result.unwrap().body, "Updated");
    support::assert_request(&request, "PUT", "/issues/23.json", None);
    assert!(request.contains(r#""issue":{"description":"Updated","tracker_id":1}"#));
    assert!(!request.contains("status_id"));
}

#[test]
fn set_issue_status_puts_any_validated_status_id() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        24,
        "Title",
        "Body",
        false,
        &[],
    ))]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let summary = redmine.set_issue_status(24, 3).unwrap();
    assert_eq!(summary.number, 24);
    assert_eq!(summary.state, "open");
    let request = requests.recv().unwrap().remove(0);
    support::assert_request(&request, "PUT", "/issues/24.json", None);
    assert!(request.contains(r#""issue":{"status_id":3}"#));
    server.join().unwrap();
}

#[test]
fn journals_back_comment_create_get_and_marker_lookup() {
    let marker = "<!-- marker -->";
    let body = "<!-- marker --> comment body";
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(501, body)])),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(501, body)])),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(501, body)])),
    ]);
    let redmine = provider(base);

    let created = redmine.create_comment(21, body, marker).unwrap();
    assert_eq!(created.id, 501);
    assert_eq!(created.marker.as_deref(), Some(marker));
    assert!(created.body.is_none());
    // Note output must anchor the exact journal so audit references land on
    // #note-<id> rather than the issue top.
    assert!(
        created
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/issues/21#note-501")),
        "html_url: {:?}",
        created.html_url
    );

    let fetched = redmine.get_comment(21, 501).unwrap();
    assert_eq!(fetched.body.as_deref(), Some(body));
    assert_eq!(fetched.marker.as_deref(), Some(marker));
    assert!(
        fetched
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/issues/21#note-501")),
        "html_url: {:?}",
        fetched.html_url
    );

    let found = redmine.find_marker(21, marker).unwrap();
    assert_eq!(found.id, 501);
    assert_eq!(found.marker.as_deref(), Some(marker));

    let requests = requests.recv().unwrap();
    support::assert_request(&requests[0], "PUT", "/issues/21.json", None);
    assert!(requests[0].contains(r#""issue":{"notes":"<!-- marker --> comment body"}"#));
    for request in &requests[1..] {
        support::assert_request(request, "GET", "/issues/21.json?include=journals", None);
    }
    server.join().unwrap();
}

#[test]
fn search_paginates_and_filters_by_requested_state() {
    let first_page =
        support::issue_collection(3, 2, &[(31, "Open one", false), (32, "Closed one", true)]);
    let second_page = support::issue_collection(3, 2, &[(33, "Open two", false)]);
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(first_page),
        MockResponse::ok(second_page),
    ]);
    let redmine = provider(base);
    let issues = redmine.search_issues(Some("needle"), "open").unwrap();
    assert_eq!(
        issues.iter().map(|issue| issue.number).collect::<Vec<_>>(),
        [31, 33]
    );
    assert!(issues.iter().all(|issue| issue.state == "open"));

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    for (request, offset) in requests.iter().zip(["0", "2"]) {
        support::assert_request(request, "GET", "/issues.json?", None);
        assert!(request.contains("status_id=open"));
        assert!(request.contains("limit=100"));
        assert!(request.contains(&format!("offset={offset}")));
        assert!(request.contains("project_id=42"));
        assert!(request.contains("subject=%7Eneedle") || request.contains("subject=~needle"));
    }
    server.join().unwrap();
}
