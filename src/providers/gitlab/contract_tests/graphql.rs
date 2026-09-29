//! GraphQL transport contracts (issue 641 P3).
//!
//! Verifies `/api/graphql` URL derivation (prefix preserved), POST shape with
//! `PRIVATE-TOKEN`, and structured handling of HTTP plus GraphQL `errors[]`
//! with token redaction. All cases run against the local mock server.

use super::support::{MockResponse, TEST_TOKEN, one, sequence};
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
struct EchoData {
    echo: Option<String>,
}

fn echo_query() -> &'static str {
    "query($id: WorkItemID!) { workItem(id: $id) { id } }"
}

#[test]
fn graphql_posts_to_api_graphql_with_private_token() {
    let body = serde_json::json!({"data": {"echo": "ok"}}).to_string();
    let (result, request) = one(MockResponse::ok(body), |provider| {
        crate::providers::gitlab::graphql::execute::<EchoData>(
            &provider.http,
            echo_query(),
            serde_json::json!({"id": "gid://gitlab/WorkItem/1"}),
            "issue hierarchy get",
        )
    });
    assert_eq!(
        result.unwrap(),
        EchoData {
            echo: Some("ok".to_owned())
        }
    );
    super::support::assert_request(&request, "POST", "/api/graphql", None);
    assert!(request.contains("workItem"), "query missing: {request}");
    assert!(
        request.contains("gid://gitlab/WorkItem/1"),
        "variables missing: {request}"
    );
    assert!(
        !request.contains("/api/v4/"),
        "GraphQL must not hit REST base: {request}"
    );
}

#[test]
fn graphql_preserves_self_managed_prefix() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        serde_json::json!({"data": {"echo": "ok"}}).to_string(),
    )]);
    let provider = GitlabProvider::new(
        GitlabConfig::new(format!("{base}/gitlab/api/v4"), 42),
        TEST_TOKEN.to_owned(),
    )
    .unwrap();
    let data: EchoData = crate::providers::gitlab::graphql::execute(
        &provider.http,
        echo_query(),
        serde_json::json!({"id": "gid://gitlab/WorkItem/1"}),
        "issue hierarchy get",
    )
    .unwrap();
    assert_eq!(data.echo.as_deref(), Some("ok"));
    let requests = requests.recv().unwrap();
    assert!(
        requests[0].starts_with("POST /gitlab/api/graphql"),
        "prefix lost: {}",
        requests[0]
    );
    server.join().unwrap();
}

#[test]
fn graphql_bare_prefix_maps_to_api_graphql() {
    let http = crate::providers::gitlab::http::GitlabHttp::new(
        "https://gitlab.example/api/v4".to_owned(),
        TEST_TOKEN.to_owned(),
    )
    .unwrap();
    assert_eq!(
        http.graphql_endpoint().unwrap().as_str(),
        "https://gitlab.example/api/graphql"
    );
    let prefixed = crate::providers::gitlab::http::GitlabHttp::new(
        "https://gitlab.example/gitlab/api/v4".to_owned(),
        TEST_TOKEN.to_owned(),
    )
    .unwrap();
    assert_eq!(
        prefixed.graphql_endpoint().unwrap().as_str(),
        "https://gitlab.example/gitlab/api/graphql"
    );
}

#[test]
fn graphql_http_error_is_structured_and_redacted() {
    let body = format!(r#"{{"message":"denied for token {TEST_TOKEN}"}}"#);
    let (result, _) = one(MockResponse::status(403, body), |provider| {
        crate::providers::gitlab::graphql::execute::<EchoData>(
            &provider.http,
            echo_query(),
            serde_json::json!({"id": "gid://gitlab/WorkItem/1"}),
            "issue hierarchy get",
        )
    });
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "http");
    assert_eq!(error.json()["status"], 403);
    let rendered = error.json().to_string();
    assert!(!rendered.contains(TEST_TOKEN), "{rendered}");
    assert!(rendered.contains("[redacted]"));
}

#[test]
fn graphql_errors_array_on_http_200_is_request_and_redacted() {
    let body =
        format!(r#"{{"errors":[{{"message":"denied for token {TEST_TOKEN}"}}], "data": null}}"#);
    let (result, request) = one(MockResponse::ok(body), |provider| {
        crate::providers::gitlab::graphql::execute::<EchoData>(
            &provider.http,
            echo_query(),
            serde_json::json!({"id": "gid://gitlab/WorkItem/1"}),
            "issue hierarchy get",
        )
    });
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "request");
    assert_eq!(error.json()["operation"], "issue hierarchy get");
    let rendered = error.json().to_string();
    assert!(!rendered.contains(TEST_TOKEN), "{rendered}");
    assert!(rendered.contains("[redacted]"));
    super::support::assert_request(&request, "POST", "/api/graphql", None);
}

#[test]
fn graphql_missing_data_without_errors_is_decode() {
    let (result, _) = one(
        MockResponse::ok(r#"{"data": null}"#.to_owned()),
        |provider| {
            crate::providers::gitlab::graphql::execute::<EchoData>(
                &provider.http,
                echo_query(),
                serde_json::json!({"id": "gid://gitlab/WorkItem/1"}),
                "issue hierarchy get",
            )
        },
    );
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "decode");
}

#[test]
fn graphql_never_puts_token_in_url() {
    let body = serde_json::json!({"data": {"echo": "ok"}}).to_string();
    let (_, request) = one(MockResponse::ok(body), |provider| {
        crate::providers::gitlab::graphql::execute::<EchoData>(
            &provider.http,
            echo_query(),
            serde_json::json!({"id": "gid://gitlab/WorkItem/1"}),
            "issue hierarchy get",
        )
    });
    let request_line = request.lines().next().unwrap_or_default();
    assert!(
        !request_line.contains(TEST_TOKEN),
        "token in URL: {request_line}"
    );
    assert!(
        !request.to_ascii_lowercase().contains("authorization:"),
        "leaked Authorization header: {request}"
    );
}
