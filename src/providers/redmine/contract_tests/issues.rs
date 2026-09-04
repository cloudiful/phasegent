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
    // Bounded single-page semantics: Redmine uses limit/offset for exactly
    // one page (default 50) and returns an envelope with total/has_more.
    let first_page =
        support::issue_collection(3, 2, &[(31, "Open one", false), (32, "Closed one", true)]);
    let (base, requests, server) = sequence(vec![MockResponse::ok(first_page)]);
    let redmine = provider(base);
    let options = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "open".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let result = redmine.search_issues(&options).unwrap();
    assert_eq!(
        result.items.iter().map(|issue| issue.number).collect::<Vec<_>>(),
        [31]
    );
    assert!(result.items.iter().all(|issue| issue.state == "open"));
    assert_eq!(result.page, 1);
    assert_eq!(result.limit, 50);
    assert_eq!(result.total_count, Some(3));
    assert!(result.has_more);
    // compact output omits bodies
    assert!(result.items.iter().all(|item| item.body.is_none()));

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    support::assert_request(request, "GET", "/issues.json?", None);
    assert!(request.contains("status_id=open"));
    assert!(request.contains("limit=50"));
    assert!(request.contains("offset=0"));
    assert!(request.contains("project_id=42"));
    assert!(request.contains("subject=%7Eneedle") || request.contains("subject=~needle"));
    server.join().unwrap();
}

#[test]
fn search_uses_limit_offset_and_validates_bounds() {
    // page 2 with limit 10 => offset 10, bounded single request
    let page = support::issue_collection(1, 1, &[(99, "Single", false)]);
    let (base, requests, server) = sequence(vec![MockResponse::ok(page)]);
    let redmine = provider(base);
    let options = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "all".to_owned(),
        page: 2,
        limit: 10,
        include_body: true,
        all: false,
    };
    let result = redmine.search_issues(&options).unwrap();
    assert_eq!(result.page, 2);
    assert_eq!(result.limit, 10);
    assert_eq!(result.items.len(), 1);
    assert!(result.items[0].body.is_some());
    let requests = requests.recv().unwrap();
    assert!(requests[0].contains("limit=10"));
    assert!(requests[0].contains("offset=10"));
    server.join().unwrap();

    // invalid bounds are rejected before any request
    let bad = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "all".to_owned(),
        page: 0,
        limit: 50,
        include_body: false,
        all: false,
    };
    assert!(redmine.search_issues(&bad).is_err());
    let bad_limit = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 200,
        include_body: false,
        all: false,
    };
    assert!(redmine.search_issues(&bad_limit).is_err());
}

#[test]
fn search_rejects_empty_query_unless_all_and_reports_truncation() {
    let redmine = provider("http://127.0.0.1:1".to_owned());
    let empty = crate::providers::IssueSearchOptions {
        query: Some("   ".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    assert!(redmine.search_issues(&empty).is_err());

    // explicit bounded all-issues mode allows empty query
    let all_options = crate::providers::IssueSearchOptions {
        query: None,
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: true,
    };
    let (result, request) = support::one(
        MockResponse::ok(support::issue_collection(0, 0, &[])),
        |redmine| redmine.search_issues(&all_options),
    );
    assert!(result.is_ok());
    assert!(!request.contains("subject="));

    // body truncation on explicit include
    let long_desc = "x".repeat(crate::providers::ISSUE_SEARCH_MAX_BODY_BYTES + 5);
    let collection = serde_json::json!({
        "total_count": 1,
        "limit": 1,
        "issues": [{
            "id": 77,
            "subject": "Long",
            "description": long_desc,
            "status": {"name": "New", "is_closed": false}
        }]
    })
    .to_string();
    let (base, requests, server) = sequence(vec![MockResponse::ok(collection)]);
    let redmine = crate::providers::RedmineProvider::new(
        crate::providers::RedmineConfig::new(base, "42", 5),
        super::support::TEST_API_KEY.to_owned(),
    )
    .unwrap();
    let trunc_options = crate::providers::IssueSearchOptions {
        query: Some("Long".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: true,
        all: false,
    };
    let result = redmine.search_issues(&trunc_options).unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].body_truncated, Some(true));
    assert_eq!(
        result.items[0].body.as_ref().unwrap().len(),
        crate::providers::ISSUE_SEARCH_MAX_BODY_BYTES
    );
    server.join().unwrap();
    drop(requests);
}
