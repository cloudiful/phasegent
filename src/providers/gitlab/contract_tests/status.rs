//! Phase 2 contract tests for `GitlabProvider::list_issue_statuses`.
//!
//! The static `WORKFLOW_LABELS` catalogue has no HTTP traffic, so the
//! tests focus on the order, the closed flag, and the round-trip with
//! the managed label mapping. The dispatcher-level coverage lives in
//! `phase2_read.rs`.

#![allow(unused_imports)]
use super::support::*;
use crate::providers::RedmineMetadataProvider;

#[test]
fn list_issue_statuses_via_provider_returns_static_workflow_catalogue() {
    let provider = provider("http://127.0.0.1:1".to_owned());
    let statuses = provider.list_issue_statuses().unwrap();
    assert_eq!(statuses.len(), 8);
    // Closed / Cancelled are the only closed entries; every other
    // status stays open so the canonical workflow policy sees the
    // same open/closed split the Redmine catalogue exposes.
    let closed: Vec<&str> = statuses
        .iter()
        .filter(|status| status.is_closed)
        .map(|status| status.name.as_str())
        .collect();
    assert_eq!(closed, vec!["Closed", "Cancelled"]);
}

#[test]
fn list_issue_statuses_via_provider_does_not_perform_http_traffic() {
    // Pointing at `127.0.0.1:1` is the project's "no server"
    // sentinel — a real HTTP request would fail to connect. The
    // static catalogue must therefore not perform any network call.
    let provider = provider("http://127.0.0.1:1".to_owned());
    let statuses = provider.list_issue_statuses().unwrap();
    assert_eq!(statuses.len(), 8);
}
