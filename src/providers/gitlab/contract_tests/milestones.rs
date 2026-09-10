//! Phase 2 contract tests for `GitlabProvider::list_milestones`.
//!
//! Mirrors the dispatcher-level coverage in `phase2_read.rs` so the
//! HTTP shape (path, query) is locked down at the provider level too.
//! The mapping tests live next to the `From<ApiMilestone> for
//! RedmineVersion` impl in `src/providers/gitlab/impl/milestones.rs`.

#![allow(unused_imports)]
use super::support::*;

#[test]
fn list_milestones_returns_empty_catalogue_for_empty_array() {
    let (result, request) = one(MockResponse::ok("[]"), |provider| {
        provider.list_milestones()
    });
    let versions = result.unwrap();
    assert!(versions.is_empty());
    assert_request(&request, "GET", "/api/v4/projects/42/milestones?", None);
}

#[test]
fn list_milestones_walks_x_next_page_when_more_pages_exist() {
    let first = MockResponse::ok(format!(
        "[{}]",
        serde_json::json!({
            "id": 7,
            "iid": 1,
            "title": "M1",
            "state": "active",
            "due_date": null,
            "start_date": null,
            "web_url": null,
            "description": null
        })
    ))
    .with_header("x-next-page", "2");
    let second = MockResponse::ok(format!(
        "[{}]",
        serde_json::json!({
            "id": 8,
            "iid": 2,
            "title": "M2",
            "state": "closed",
            "due_date": "2026-09-30",
            "start_date": null,
            "web_url": null,
            "description": null
        })
    ))
    .with_header("x-next-page", "");
    let (base, requests, server) = sequence(vec![first, second]);
    let provider = provider(base);
    let versions = provider.list_milestones().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].id, 7);
    assert_eq!(versions[0].name, "M1");
    assert_eq!(versions[0].status, "active");
    assert_eq!(versions[1].id, 8);
    assert_eq!(versions[1].name, "M2");
    assert_eq!(versions[1].status, "closed");
    assert_eq!(versions[1].due_date.as_deref(), Some("2026-09-30"));
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    for (idx, request) in requests.iter().enumerate() {
        assert!(
            request.starts_with("GET /api/v4/projects/42/milestones?"),
            "page {idx} request: {request}",
        );
        assert!(
            request.contains(&format!("page={}", idx + 1)),
            "page {idx} missing page number: {request}",
        );
    }
    server.join().unwrap();
}
