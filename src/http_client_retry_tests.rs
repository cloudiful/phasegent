use super::{ForgejoConfig, ForgejoProvider, GitlabHttp, MockResponse, sequence};
use crate::http_client;
use crate::redmine_http::RedmineHttp;
use std::time::{Duration, Instant};

pub(super) fn safe_get_retries_on_503_then_succeeds() {
    let (base, requests, server) = sequence(vec![
        MockResponse::status(503, r#"{"message":"try again"}"#),
        MockResponse::json(r#"{"id":7,"number":7,"title":"ok","body":"","state":"open"}"#),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner", "repo"), "token".into()).unwrap();
    let start = Instant::now();
    let issue = provider.get_issue(7).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(issue.number, 7);
    assert!(elapsed < Duration::from_secs(2), "elapsed={elapsed:?}");
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 2, "must have retried once, got {:?}", reqs);
    assert!(reqs[0].contains("/repos/owner/repo/issues/7"));
    assert!(reqs[1].contains("/repos/owner/repo/issues/7"));
    server.join().unwrap();
}

pub(super) fn safe_get_retries_on_429_with_retry_after() {
    let (base, requests, server) = sequence(vec![
        MockResponse::status(429, r#"{"message":"rate"}"#).with_header("Retry-After", "1"),
        MockResponse::json(
            r#"{"id":1,"iid":1,"title":"ok","description":"","state":"opened","labels":[]}"#,
        )
        .with_header("x-next-page", ""),
    ]);
    let gitlab = GitlabHttp::new(format!("{base}/api/v4"), "glpat-test".into()).unwrap();
    let start = Instant::now();
    let issues = gitlab
        .get::<serde_json::Value>("projects/42/issues/1", &[], "issue get")
        .unwrap();
    let elapsed = start.elapsed();
    assert!(issues.get("title").is_some());
    assert!(elapsed >= Duration::from_millis(900), "elapsed={elapsed:?}");
    assert!(elapsed < Duration::from_secs(3), "elapsed={elapsed:?}");
    assert_eq!(requests.recv().unwrap().len(), 2);
    server.join().unwrap();
}

pub(super) fn retry_after_is_capped_at_2s() {
    let (base, _requests, server) = sequence(vec![
        MockResponse::status(429, r#"{"message":"rate"}"#).with_header("Retry-After", "10"),
        MockResponse::json(r#"{"id":1,"number":7,"title":"ok","body":"","state":"open"}"#),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner", "repo"), "token".into()).unwrap();
    let start = Instant::now();
    let issue = provider.get_issue(7).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(issue.number, 7);
    assert!(elapsed < Duration::from_secs(4), "elapsed={elapsed:?}");
    assert!(
        elapsed >= Duration::from_millis(1900),
        "elapsed={elapsed:?}"
    );
    server.join().unwrap();
}

pub(super) fn post_and_4xx_are_not_retried() {
    let (base, requests, server) =
        sequence(vec![MockResponse::status(503, r#"{"message":"oops"}"#)]);
    let gitlab = GitlabHttp::new(format!("{base}/api/v4"), "glpat-test".into()).unwrap();
    let payload = serde_json::json!({"title":"t","description":"d","labels":[]});
    let err = gitlab
        .post::<serde_json::Value, _>("projects/42/issues", &payload, "issue create")
        .unwrap_err();
    assert_eq!(err.json()["kind"], "http");
    assert_eq!(requests.recv().unwrap().len(), 1);
    server.join().unwrap();

    let (base, requests, server) =
        sequence(vec![MockResponse::status(400, r#"{"message":"bad"}"#)]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner", "repo"), "token".into()).unwrap();
    let err = provider.get_issue(7).unwrap_err();
    assert_eq!(err.json()["kind"], "http");
    assert_eq!(err.json()["status"], 400);
    assert_eq!(requests.recv().unwrap().len(), 1);
    server.join().unwrap();
}

pub(super) fn retry_requires_cloneable_request() {
    let (base, requests, server) =
        sequence(vec![MockResponse::status(503, r#"{"message":"retry?"}"#)]);
    let client = http_client::build_client().unwrap();
    let body = reqwest::blocking::Body::new(std::io::empty());
    let builder = client.get(format!("{base}/non-cloneable")).body(body);
    assert!(builder.try_clone().is_none());
    let result = http_client::fetch_with_retry(builder, "op", |m| m.to_owned());
    let (status, _, _) = result.unwrap();
    assert_eq!(status.as_u16(), 503);
    assert_eq!(requests.recv().unwrap().len(), 1);
    server.join().unwrap();
}

pub(super) fn redmine_get_retries_on_502() {
    let (base, requests, server) = sequence(vec![
        MockResponse::status(502, r#"{"errors":["bad gateway"]}"#),
        MockResponse::json(
            r#"{"issue":{"id":7,"subject":"ok","description":"","status":{"id":1,"name":"New"}}}"#,
        ),
    ]);
    let redmine = RedmineHttp::new(base, "secret-key".into()).unwrap();
    let issue: serde_json::Value = redmine.get("issues/7.json", &[], "issue get").unwrap();
    assert!(issue.get("issue").is_some());
    assert_eq!(requests.recv().unwrap().len(), 2);
    server.join().unwrap();
}

pub(super) fn gitlab_get_page_retries_on_504() {
    let (base, requests, server) = sequence(vec![
        MockResponse::status(504, r#"{"message":"timeout"}"#),
        MockResponse::json(
            r#"[{"id":1,"iid":1,"title":"a","description":"","state":"opened","labels":[]}]"#,
        )
        .with_header("x-next-page", ""),
    ]);
    let gitlab = GitlabHttp::new(format!("{base}/api/v4"), "glpat-test".into()).unwrap();
    let (items, _, _) = gitlab
        .get_page::<serde_json::Value>("projects/42/issues", &[], "issue search")
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(requests.recv().unwrap().len(), 2);
    server.join().unwrap();
}

#[test]
fn safe_get_retries_on_503_then_succeeds_test() {
    safe_get_retries_on_503_then_succeeds();
}

#[test]
fn safe_get_retries_on_429_with_retry_after_test() {
    safe_get_retries_on_429_with_retry_after();
}

#[test]
fn retry_after_is_capped_at_2s_test() {
    retry_after_is_capped_at_2s();
}

#[test]
fn post_and_4xx_are_not_retried_test() {
    post_and_4xx_are_not_retried();
}

#[test]
fn retry_requires_cloneable_request_test() {
    retry_requires_cloneable_request();
}

#[test]
fn redmine_get_retries_on_502_test() {
    redmine_get_retries_on_502();
}

#[test]
fn gitlab_get_page_retries_on_504_test() {
    gitlab_get_page_retries_on_504();
}
