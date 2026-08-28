#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{CiProvider, IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn get_issue_hits_project_issues_iid_with_private_token() {
    let (result, request) = one(
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
        |provider| provider.get_issue(7),
    );
    let issue = result.unwrap();
    assert_eq!(issue.number, 7);
    assert_eq!(issue.title, "Title");
    assert_eq!(issue.state, "open");
    assert_request(&request, "GET", "/api/v4/projects/42/issues/7", None);
}

#[test]
fn search_issues_open_sends_state_opened_and_paginates() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!("[{}]", issue_payload(1, "One", "opened", &[])))
            .with_header("x-next-page", "2"),
        MockResponse::ok(format!("[{}]", issue_payload(2, "Two", "opened", &[])))
            .with_header("x-next-page", ""),
    ]);
    let dispatcher = dispatcher(base);
    let issues = dispatcher.search_issues(Some("needle"), "open").unwrap();
    assert_eq!(issues.len(), 2);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("state=opened"));
    assert!(requests[0].contains("search=needle"));
    assert!(requests[0].contains("page=1"));
    assert!(requests[1].contains("page=2"));
    for request in &requests {
        assert!(
            request.starts_with("GET /api/v4/projects/42/issues?"),
            "request: {request}"
        );
    }
    server.join().unwrap();
}

#[test]
fn search_issues_closed_maps_to_state_closed() {
    let (result, request) = one(
        MockResponse::ok(format!("[{}]", issue_payload(3, "Done", "closed", &[]))),
        |provider| provider.search_issues(None, "closed"),
    );
    let issues = result.unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].state, "closed");
    assert!(request.contains("state=closed"));
    assert!(!request.contains("state=opened"));
}

#[test]
fn search_issues_all_omits_state_filter() {
    let (result, request) = one(
        MockResponse::ok("[]").with_header("x-next-page", ""),
        |provider| provider.search_issues(None, "all"),
    );
    assert!(result.unwrap().is_empty());
    assert!(
        !request.contains("state="),
        "all must not send a state filter: {request}"
    );
}

#[test]
fn search_issues_rejects_unknown_state_before_request() {
    let result = zero_request(|provider| provider.search_issues(None, "bogus"));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn repeated_non_empty_search_page_returns_pagination_error() {
    let (base, requests, server) = sequence(vec![
        // Both pages have x-next-page so the helper must walk past
        // page 1; the repeated body on page 2 is what should trip
        // the safety guard.
        MockResponse::ok(format!("[{}]", issue_payload(1, "One", "opened", &[])))
            .with_header("x-next-page", "2"),
        MockResponse::ok(format!("[{}]", issue_payload(1, "One", "opened", &[])))
            .with_header("x-next-page", "3"),
    ]);
    let dispatcher = dispatcher(base);
    let error = dispatcher.search_issues(None, "all").unwrap_err();
    assert_eq!(error.json()["kind"], "pagination");
    assert_eq!(requests.recv().unwrap().len(), 2);
    server.join().unwrap();
}

#[test]
fn create_issue_posts_to_issues_with_labels() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_payload(
        11,
        "Created",
        "opened",
        &["type::bug"],
    ))]);
    let provider = provider(base);
    let labels = vec!["type::bug".to_owned()];
    let issue = provider
        .create_issue_with_labels("Created", "Body", &labels)
        .unwrap();
    assert_eq!(issue.number, 11);
    let request = requests.recv().unwrap().remove(0);
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues",
        Some(r#""title":"Created""#),
    );
    assert!(request.contains(r#""description":"Body""#));
    assert!(request.contains(r#""labels":["type::bug"]"#));
    server.join().unwrap();
}

#[test]
fn update_body_with_labels_uses_put_and_emits_add_labels() {
    let (base, requests, server) = sequence(vec![
        // ensure_labels first GETs the project label list; return
        // an empty list so the provider must create type::feature.
        MockResponse::ok("[]").with_header("x-next-page", ""),
        // POST the new type::feature label so ensure_labels succeeds.
        MockResponse::ok(label_payload(50, "type::feature")),
        // The provider then GETs the current issue so it can see
        // the existing label set before deciding which managed
        // tracker label to remove.
        MockResponse::ok(issue_payload(12, "Title", "opened", &["type::bug"])),
        MockResponse::ok(issue_payload(12, "Title", "opened", &["type::feature"])),
    ]);
    let provider = provider(base);
    let labels = vec!["type::feature".to_owned()];
    let issue = provider
        .update_body_with_labels(12, "Updated body", &labels)
        .unwrap();
    assert_eq!(issue.number, 12);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/12"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/12"));
    assert!(requests[3].contains(r#""description":"Updated body""#));
    assert!(requests[3].contains(r#""add_labels":["type::feature"]"#));
    // The previous tracker label is removed in the same payload so
    // the issue never carries both type::bug and type::feature.
    assert!(requests[3].contains(r#""remove_labels":["type::bug"]"#));
    server.join().unwrap();
}

#[test]
fn http_error_redacts_token_from_response_body() {
    let body = format!(r#"{{"message":"denied for token {TEST_TOKEN}"}}"#);
    let (result, _request) = one(MockResponse::status(403, body), |provider| {
        provider.get_issue(7)
    });
    let error = result.unwrap_err();
    let rendered = error.json().to_string();
    assert!(!rendered.contains(TEST_TOKEN), "{rendered}");
    assert!(rendered.contains("[redacted]"));
    assert_eq!(error.json()["status"], 403);
    assert_eq!(error.json()["operation"], "issue get");
}

#[test]
fn decode_error_does_not_leak_token() {
    // An unparseable response surfaces a `decode` error. Serde_json's
    // own parse-error string never includes the raw input, so the
    // contract is simply that the error kind is `decode` and the
    // token does not surface anywhere in the rendered payload.
    let body = format!("not-json {TEST_TOKEN}");
    let (result, _request) = one(MockResponse::ok(body), |provider| provider.get_issue(7));
    let error = result.unwrap_err();
    let rendered = error.json().to_string();
    assert_eq!(error.json()["kind"], "decode");
    assert_eq!(error.json()["operation"], "issue get");
    assert!(!rendered.contains(TEST_TOKEN), "{rendered}");
}

#[test]
fn dispatcher_routes_gitlab_get_issue_to_gitlab_provider() {
    let (result, request) = one(
        MockResponse::ok(issue_payload(33, "Routed", "opened", &[])),
        |provider| provider.get_issue(33),
    );
    let issue = result.unwrap();
    assert_eq!(issue.number, 33);
    assert_eq!(issue.title, "Routed");
    assert_request(&request, "GET", "/api/v4/projects/42/issues/33", None);
}
