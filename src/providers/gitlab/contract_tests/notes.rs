#![allow(unused_imports)]
use super::support::*;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{CiProvider, IssueProvider, ProviderDispatcher, RepoProvider};

#[test]
fn create_note_posts_to_notes_and_returns_stable_url() {
    // The create_note path POSTs the note first, then GETs the
    // parent issue so it can build a browsable
    // `<issue_web_url>#note_<id>` URL rather than the legacy
    // `/api/v4` API path.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(note_payload(42, "<!-- marker --> body")),
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
    ]);
    let provider = provider(base);
    let comment = provider.create_note(7, "<!-- marker --> body").unwrap();
    assert_eq!(comment.id, 42);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/7/notes"));
    assert!(requests[0].contains(r#""body":"<!-- marker --> body""#));
    assert!(requests[1].starts_with("GET /api/v4/projects/42/issues/7"));
    assert_eq!(
        comment.html_url.as_deref(),
        Some("https://gitlab.example/group/project/-/issues/7#note_42"),
    );
    server.join().unwrap();
}

#[test]
fn get_note_hits_notes_note_id() {
    // The get_note path issues the note GET first, then the
    // parent issue GET used to build the browsable URL.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(note_payload(42, "<!-- marker --> body")),
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
    ]);
    let provider = provider(base);
    let comment = provider.get_note(7, 42).unwrap();
    assert_eq!(comment.id, 42);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7/notes/42"));
    assert!(requests[1].starts_with("GET /api/v4/projects/42/issues/7"));
    assert_eq!(
        comment.html_url.as_deref(),
        Some("https://gitlab.example/group/project/-/issues/7#note_42"),
    );
    server.join().unwrap();
}

#[test]
fn find_marker_paginates_notes_until_match() {
    let (base, requests, server) = sequence(vec![
        // GET parent issue so the note URL can be derived from the
        // parent issue's web_url.
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
        MockResponse::ok(format!("[{}]", note_payload(1, "first body")))
            .with_header("x-next-page", "2"),
        MockResponse::ok(format!("[{}]", note_payload(2, "second body with marker")))
            .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let comment = provider.find_marker(7, "marker").unwrap();
    assert_eq!(comment.id, 2);
    assert_eq!(comment.marker.as_deref(), Some("marker"));
    assert_eq!(
        comment.html_url.as_deref(),
        Some("https://gitlab.example/group/project/-/issues/7#note_2"),
    );
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7"));
    assert!(requests[1].contains("page=1"));
    assert!(requests[2].contains("page=2"));
    server.join().unwrap();
}

#[test]
fn find_marker_returns_not_found_when_no_note_matches() {
    let (base, _requests, server) = sequence(vec![
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
        MockResponse::ok(format!("[{}]", note_payload(1, "first body")))
            .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let error = provider.find_marker(7, "missing").unwrap_err();
    assert_eq!(error.json()["kind"], "not_found");
    server.join().unwrap();
}

#[test]
fn find_marker_rejects_empty_marker_before_request() {
    let result = zero_request(|provider| provider.find_marker(7, ""));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn note_html_url_is_browsable_web_url_with_note_fragment() {
    // GitLab renders notes inline on the parent issue page, so the
    // canonical note URL is `<issue_web_url>#note_<id>`. The web_url
    // returned by GitLab is `<host>/<namespace>/<project>/-/issues/<iid>`;
    // the provider must NOT synthesise an `/api/v4` path that is not
    // browsable from a web browser.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(note_payload(99, "note body")),
        MockResponse::ok(issue_payload(8, "Title", "opened", &[])),
    ]);
    let provider = provider(base);
    let comment = provider.create_note(8, "note body").unwrap();
    let url = comment
        .html_url
        .as_deref()
        .expect("note should carry a web URL");
    assert_eq!(
        url,
        "https://gitlab.example/group/project/-/issues/8#note_99",
    );
    assert!(
        !url.contains("/api/v4"),
        "note URL must not be an API-path URL: {url}",
    );
    assert!(
        !url.contains("/notes/"),
        "note URL must not embed the API /notes/ path: {url}"
    );
    // The PUT payload still went to the notes endpoint.
    let requests = requests.recv().unwrap();
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/8/notes"));
    assert!(requests[1].starts_with("GET /api/v4/projects/42/issues/8"));
    server.join().unwrap();
}

#[test]
fn note_html_url_is_none_when_issue_web_url_is_missing() {
    // If the parent issue payload omits `web_url`, the note must
    // also report `html_url = None` rather than fall back to an
    // `/api/v4` API path that is not browsable.
    let issue_body_without_web_url = serde_json::json!({
        "id": 9,
        "iid": 9,
        "title": "Title",
        "description": "",
        "state": "opened",
        "labels": [],
        // Deliberately omit `web_url`.
    })
    .to_string();
    let (base, _requests, server) = sequence(vec![
        MockResponse::ok(note_payload(123, "note body")),
        MockResponse::ok(issue_body_without_web_url),
    ]);
    let provider = provider(base);
    let comment = provider.create_note(9, "note body").unwrap();
    assert!(
        comment.html_url.is_none(),
        "missing issue web_url must produce a None note URL; got {:?}",
        comment.html_url,
    );
    server.join().unwrap();
}

#[test]
fn find_marker_empty_marker_returns_config_error_without_request() {
    let result = zero_request(|provider| provider.find_marker(7, ""));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}
