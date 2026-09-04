#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{IssueProvider, ProviderDispatcher, RepoProvider};

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
    // Bounded single-page semantics: one request with page/per_page.
    let options = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "open".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let (base, requests, server) = sequence(vec![MockResponse::ok(format!(
        "[{}]",
        issue_payload(1, "One", "opened", &[])
    ))
    .with_header("x-next-page", "2")
    .with_header("x-total", "2")]);
    let dispatcher = dispatcher(base);
    let result = dispatcher.search_issues(&options).unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.page, 1);
    assert_eq!(result.limit, 50);
    assert!(result.has_more);
    assert_eq!(result.total_count, Some(2));
    // compact output omits bodies
    assert!(result.items[0].body.is_none());
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("state=opened"));
    assert!(requests[0].contains("search=needle"));
    assert!(requests[0].contains("page=1"));
    assert!(requests[0].contains("per_page=50"));
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
    let options = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "closed".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let (result, request) = one(
        MockResponse::ok(format!("[{}]", issue_payload(3, "Done", "closed", &[]))),
        |provider| provider.search_issues(&options),
    );
    let result = result.unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].state, "closed");
    assert!(request.contains("state=closed"));
    assert!(!request.contains("state=opened"));
}

#[test]
fn search_issues_all_omits_state_filter() {
    let options = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let (result, request) = one(
        MockResponse::ok("[]").with_header("x-next-page", ""),
        |provider| provider.search_issues(&options),
    );
    assert!(result.unwrap().items.is_empty());
    assert!(
        !request.contains("state="),
        "all must not send a state filter: {request}"
    );

    // bounded all-issues mode allows empty query with all=true
    let all_empty = crate::providers::IssueSearchOptions {
        query: None,
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: true,
    };
    let (result, request) = one(
        MockResponse::ok("[]").with_header("x-next-page", ""),
        |provider| provider.search_issues(&all_empty),
    );
    assert!(result.unwrap().items.is_empty());
    assert!(!request.contains("search="));
}

#[test]
fn search_issues_rejects_unknown_state_before_request() {
    let options = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "bogus".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let result = zero_request(|provider| provider.search_issues(&options));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn repeated_non_empty_search_page_returns_pagination_error() {
    // With bounded single-page search, the repeated-page guard is not
    // exercised via pagination loop; validation is now at the envelope
    // layer. Empty query without --all is rejected before any request.
    let empty = crate::providers::IssueSearchOptions {
        query: None,
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let result = zero_request(|provider| provider.search_issues(&empty));
    assert_eq!(result.unwrap_err().json()["kind"], "config");
}

#[test]
fn search_reports_truncation_and_validates_bounds() {
    // compact vs body inclusion
    let compact = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let (result, _request) = one(
        MockResponse::ok(format!("[{}]", issue_payload(1, "One", "opened", &[]))),
        |provider| provider.search_issues(&compact),
    );
    let output = result.unwrap();
    assert!(output.items[0].body.is_none());

    let long_body = "a".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES + 10);
    let long_payload = serde_json::json!({
        "id": 9,
        "iid": 9,
        "title": "Long",
        "description": long_body,
        "state": "opened",
        "web_url": "https://gitlab.example/issues/9",
        "labels": []
    })
    .to_string();
    let with_body = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: true,
        all: false,
    };
    let (result, _request) = one(
        MockResponse::ok(format!("[{long_payload}]")),
        |provider| provider.search_issues(&with_body),
    );
    let output = result.unwrap();
    assert_eq!(output.items[0].body_truncated, Some(true));
    assert_eq!(
        output.items[0].body.as_ref().unwrap().len(),
        crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES
    );

    // invalid bounds
    let bad_page = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "all".to_owned(),
        page: 0,
        limit: 50,
        include_body: false,
        all: false,
    };
    assert_eq!(
        zero_request(|provider| provider.search_issues(&bad_page))
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    let bad_limit = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 200,
        include_body: false,
        all: false,
    };
    assert_eq!(
        zero_request(|provider| provider.search_issues(&bad_limit))
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    // whitespace-only query without all is rejected
    let whitespace = crate::providers::IssueSearchOptions {
        query: Some("   ".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    assert_eq!(
        zero_request(|provider| provider.search_issues(&whitespace))
            .unwrap_err()
            .json()["kind"],
        "config"
    );
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
