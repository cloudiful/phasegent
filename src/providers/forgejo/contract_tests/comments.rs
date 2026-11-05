use super::super::{ForgejoProvider, config::ForgejoConfig};
use super::support::server;
use crate::providers::IssueProvider;

const REPO: &str = r#"{"id":17,"name":"repo","full_name":"owner/repo"}"#;
const ISSUE: &str = r#"{"id":501,"number":7,"title":"example","state":"open"}"#;
const COMMENT: &str = r#"{"id":83,"body":"hello marker-1","html_url":"https://example.test/c/83","issue_url":"{BASE}/api/v1/repos/owner/repo/issues/7"}"#;

#[test]
fn creates_and_finds_marker_with_native_comment_id() {
    let (base, server) = server::sequence(vec![
        REPO.into(),
        ISSUE.into(),
        COMMENT.into(),
        ISSUE.into(),
        format!("[{COMMENT}]"),
        "[]".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let created = provider
        .create_comment(7, "hello marker-1", "marker-1")
        .unwrap();
    assert_eq!(created.id, 83);
    assert_eq!(provider.find_marker(7, "marker-1").unwrap().id, 83);
    let requests = server.join().unwrap();
    assert!(requests[2].starts_with("POST /api/v1/repos/owner/repo/issues/7/comments "));
    assert!(requests[2].ends_with(r#"{"body":"hello marker-1"}"#));
}

#[test]
fn gets_and_pages_comments_in_issue_scope() {
    let (base, server) = server::sequence(vec![
        REPO.into(),
        ISSUE.into(),
        format!("[{COMMENT}]"),
        "[]".into(),
        ISSUE.into(),
        format!("[{COMMENT}]"),
        "[]".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let got = provider.get_comment(7, 83).unwrap();
    assert_eq!(got.body.as_deref(), Some("hello marker-1"));
    let page = provider
        .list_comments_page(7, crate::pagination::PageRequest::new(1, 1).unwrap())
        .unwrap();
    assert_eq!(
        page.items[0].html_url.as_deref(),
        Some("https://example.test/c/83")
    );
    assert!(!page.has_more);
    assert_eq!(server.join().unwrap().len(), 7);
}

#[test]
fn wrong_comment_parent_is_rejected_without_fetching_external_url() {
    let wrong = COMMENT.replace("issues/7", "issues/8");
    let (base, server) = server::sequence(vec![
        REPO.into(),
        ISSUE.into(),
        format!("[{wrong}]"),
        "[]".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.list_comments(7).is_err());
    assert_eq!(server.join().unwrap().len(), 4);
}
