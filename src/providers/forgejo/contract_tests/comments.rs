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
        COMMENT.into(),
        ISSUE.into(),
        format!("[{COMMENT}]"),
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
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests[2].starts_with("GET /api/v1/repos/owner/repo/issues/comments/83 "));
    assert!(requests[4].starts_with("GET /api/v1/repos/owner/repo/issues/7/comments "));
}

#[test]
fn wrong_comment_parent_is_rejected_without_fetching_external_url() {
    let wrong = COMMENT.replace("issues/7", "issues/8");
    let (base, server) = server::sequence(vec![REPO.into(), ISSUE.into(), format!("[{wrong}]")]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.list_comments(7).is_err());
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn direct_reads_reject_missing_and_wrong_parent() {
    for (status, body) in [
        ("204 No Content", "".to_owned()),
        ("404 Not Found", "".to_owned()),
        ("200 OK", COMMENT.replace("issues/7", "issues/8")),
    ] {
        let (base, server) = server::sequence_with_headers(vec![
            (REPO.into(), vec![]),
            (ISSUE.into(), vec![]),
            (body, vec![(":status".into(), status.into())]),
        ]);
        let provider =
            ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture")
                .unwrap();
        assert!(matches!(
            provider.get_comment(7, 83),
            Err(crate::providers::api::PhasegentError::NotFound { .. })
        ));
        assert_eq!(server.join().unwrap().len(), 3);
    }
}

#[test]
fn nonpaged_collection_accepts_101_and_rejects_incomplete_totals() {
    let comments = format!(
        "[{}]",
        (1..=101)
            .map(|id| COMMENT.replace("\"id\":83", &format!("\"id\":{id}")))
            .collect::<Vec<_>>()
            .join(",")
    );
    for (total, succeeds) in [("101", true), ("102", false), ("invalid", false)] {
        let (base, server) = server::sequence_with_headers(vec![
            (REPO.into(), vec![]),
            (ISSUE.into(), vec![]),
            (
                comments.clone(),
                vec![("X-Total-Count".into(), total.into())],
            ),
        ]);
        let provider =
            ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture")
                .unwrap();
        let result = provider.list_comments(7);
        assert_eq!(result.is_ok(), succeeds);
        if let Ok(items) = result {
            assert_eq!(items.len(), 101);
        }
        let requests = server.join().unwrap();
        assert!(requests[2].starts_with("GET /api/v1/repos/owner/repo/issues/7/comments "));
    }
}

#[test]
fn accepts_the_exact_html_parent_from_the_trusted_instance() {
    let comment = COMMENT.replace(
        "{BASE}/api/v1/repos/owner/repo/issues/7",
        "{BASE}/owner/repo/issues/7",
    );
    let (base, server) = server::sequence(vec![REPO.into(), ISSUE.into(), format!("[{comment}]")]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let comments = provider.list_comments(7).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn rejects_non_exact_comment_parents_without_using_the_association_url() {
    let cases = [
        (
            "wrong host",
            "https://evil.test/owner/repo/issues/7".to_owned(),
        ),
        ("wrong repository", "{BASE}/other/repo/issues/7".to_owned()),
        ("wrong issue", "{BASE}/owner/repo/issues/8".to_owned()),
        ("query", "{BASE}/owner/repo/issues/7?x=1".to_owned()),
        ("fragment", "{BASE}/owner/repo/issues/7#notes".to_owned()),
        (
            "userinfo and foreign host",
            "https://user@example.test/owner/repo/issues/7".to_owned(),
        ),
        (
            "external issue",
            "https://example.test/external/issues/7".to_owned(),
        ),
    ];
    for (label, association) in cases {
        let comment = COMMENT.replace("{BASE}/api/v1/repos/owner/repo/issues/7", &association);
        let (base, server) =
            server::sequence(vec![REPO.into(), ISSUE.into(), format!("[{comment}]")]);
        let provider =
            ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture")
                .unwrap();
        assert!(provider.list_comments(7).is_err(), "{label}");
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3, "{label}: {requests:?}");
    }
    let zero = COMMENT.replace("\"id\":83", "\"id\":0");
    let (base, server) = server::sequence(vec![REPO.into(), ISSUE.into(), format!("[{zero}]")]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.list_comments(7).is_err(), "zero native id");
    assert_eq!(server.join().unwrap().len(), 3);
}
