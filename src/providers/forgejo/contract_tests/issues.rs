use super::super::{ForgejoProvider, config::ForgejoConfig};
use super::support::server;
use crate::providers::{IssueProvider, IssueSearchOptions};

const REPO: &str = r#"{"id":17,"name":"repo","full_name":"owner/repo"}"#;
const ISSUE: &str = r#"{"id":501,"number":7,"title":"example","body":"text","state":"open","html_url":"https://example.test/owner/repo/issues/7"}"#;
fn resolved() -> String {
    ISSUE.replace(
        "\"state\":\"open\"",
        "\"state\":\"open\",\"labels\":[{\"id\":4,\"name\":\"status/Resolved\"}]",
    )
}

#[test]
fn scoped_issue_get_and_search_keep_global_and_local_ids() {
    let (base, server) = server::sequence(vec![
        REPO.into(),
        ISSUE.into(),
        format!("[{ISSUE}]"),
        "[]".into(),
    ]);
    let provider = ForgejoProvider::new(
        ForgejoConfig::new(base, "owner/repo").unwrap(),
        "test-token",
    )
    .unwrap();
    let issue = provider.get_issue(7).unwrap();
    assert_eq!((issue.id, issue.number), (501, 7));
    assert_eq!(
        issue.project.unwrap().identifier.as_deref(),
        Some("owner/repo")
    );
    let options = IssueSearchOptions {
        query: Some("a&b".into()),
        state: "open".into(),
        page: 1,
        limit: 5,
        include_body: true,
        all: false,
    };
    let page = provider.search_issues(&options).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].body.as_deref(), Some("text"));
    let requests = server.join().unwrap();
    assert!(requests[1].starts_with("GET /api/v1/repos/owner/repo/issues/7 "));
    assert!(requests[2].contains("type=issues&state=open&q=a%26b&page=1&limit=100"));
    assert!(requests[3].contains("page=2&limit=100"));
}

#[test]
fn search_follows_short_pages_until_empty() {
    let second = ISSUE
        .replace("\"number\":7", "\"number\":8")
        .replace("\"id\":501", "\"id\":502");
    let (base, server) = server::sequence(vec![
        REPO.into(),
        format!("[{ISSUE}]"),
        format!("[{second}]"),
        "[]".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let options = IssueSearchOptions {
        query: None,
        state: "all".into(),
        page: 2,
        limit: 1,
        include_body: false,
        all: true,
    };
    let page = provider.search_issues(&options).unwrap();
    assert_eq!(page.items[0].number, 8);
    assert_eq!(page.total_count, Some(2));
    assert!(!page.has_more);
    assert!(server.join().unwrap()[3].contains("page=3&limit=100"));
}

#[test]
fn malformed_issue_response_fails_closed() {
    let (base, server) = server::sequence(vec![REPO.into(), "{\"id\":501}".into()]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(matches!(
        provider.get_issue(7),
        Err(crate::providers::api::PhasegentError::Decode { .. })
    ));
    server.join().unwrap();
}

#[test]
fn writes_preload_and_reject_pull_requests() {
    let pr = r#"{"id":501,"number":7,"title":"pr","state":"open","pull_request":{"merged":false}}"#;
    let (base, server) = server::sequence(vec![REPO.into(), pr.into()]);
    let provider = ForgejoProvider::new(
        ForgejoConfig::new(base, "owner/repo").unwrap(),
        "test-token",
    )
    .unwrap();
    assert!(provider.close_issue(7).is_err());
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn rejects_wrong_repository_and_incorrect_issue_number() {
    let wrong_repo = REPO.replace("owner/repo", "other/repo");
    let (base, server) = server::sequence(vec![wrong_repo]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.get_issue(7).is_err());
    assert_eq!(server.join().unwrap().len(), 1);
    let (base, server) = server::sequence(vec![REPO.into(), ISSUE.into()]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.get_issue(8).is_err());
    server.join().unwrap();
}

#[test]
fn update_and_unconfirmed_close_are_not_silently_successful() {
    let (base, server) = server::sequence(vec![
        REPO.into(),
        ISSUE.into(),
        ISSUE.into(),
        resolved(),
        "[]".into(),
        resolved(),
        ISSUE.into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert_eq!(provider.update_body(7, "changed").unwrap().number, 7);
    assert!(provider.close_issue(7).is_err());
    let requests = server.join().unwrap();
    assert!(requests[2].ends_with(r#"{"body":"changed"}"#));
    assert!(requests[4].contains("/issues/7/dependencies?"));
    assert!(requests[6].ends_with(r#"{"state":"closed"}"#));
}

#[test]
fn create_posts_title_and_body_without_any_label_read() {
    let (base, server) = server::sequence(vec![REPO.into(), ISSUE.into()]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert_eq!(provider.create_issue("title", "body").unwrap().number, 7);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with("POST /api/v1/repos/owner/repo/issues "));
    assert!(requests[1].ends_with(r#"{"body":"body","title":"title"}"#));
    assert!(
        !requests.iter().any(|request| request.contains("/labels")),
        "the native create reads no label catalogue: {requests:?}"
    );
}

#[test]
fn close_runs_the_dependency_preflight_before_the_native_patch() {
    let (base, server) = server::sequence(vec![
        REPO.into(),
        resolved(),
        r#"[{"state":"open"}]"#.into(),
        "[]".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.close_issue(7).is_err());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].contains("/issues/7/dependencies?"));
    assert!(
        !requests.iter().any(|request| request.starts_with("PATCH ")),
        "an open dependency prevents the native close: {requests:?}"
    );
    assert!(
        !requests.iter().any(|request| request.contains("/labels")),
        "the native close reads no label catalogue: {requests:?}"
    );
}

#[test]
fn close_rejects_open_dependencies_before_patch() {
    let (base, server) = server::sequence(vec![
        REPO.into(),
        resolved(),
        r#"[{"state":"open"}]"#.into(),
        "[]".into(),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    assert!(provider.close_issue(7).is_err());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[test]
fn create_and_close_use_exact_native_wire_contracts() {
    let closed = resolved().replace("\"state\":\"open\"", "\"state\":\"closed\"");
    let (base, server) = server::sequence(vec![
        REPO.into(),
        ISSUE.into(),
        resolved(),
        "[]".into(),
        resolved(),
        closed,
    ]);
    let provider = ForgejoProvider::new(
        ForgejoConfig::new(base, "owner/repo").unwrap(),
        "test-token",
    )
    .unwrap();
    assert_eq!(provider.create_issue("example", "text").unwrap().number, 7);
    assert_eq!(provider.close_issue(7).unwrap().state, "closed");
    let req = server.join().unwrap();
    assert!(req[1].starts_with("POST /api/v1/repos/owner/repo/issues "));
    assert!(req[1].ends_with(r#"{"body":"text","title":"example"}"#));
    assert!(req[3].contains("/issues/7/dependencies?"));
    assert!(req[5].starts_with("PATCH /api/v1/repos/owner/repo/issues/7 "));
    assert!(req[5].ends_with(r#"{"state":"closed"}"#));
    assert!(
        !req.iter().any(|request| request.contains("/labels")),
        "the native contract never reads or writes labels: {req:?}"
    );
    assert!(
        !req.iter().any(|request| request.starts_with("PUT ")),
        "the native contract never writes labels: {req:?}"
    );
}

#[test]
fn metadata_total_stops_after_the_exact_page() {
    let (base, server) = server::sequence_with_headers(vec![
        (REPO.into(), vec![]),
        (
            format!("[{ISSUE}]"),
            vec![("X-Total-Count".into(), "1".into())],
        ),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let page = provider
        .search_issues(&IssueSearchOptions {
            query: None,
            state: "all".into(),
            page: 1,
            limit: 50,
            include_body: false,
            all: true,
        })
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.total_count, Some(1));
    assert_eq!(
        server.join().unwrap().len(),
        2,
        "the reported total must finish after the exact page without another GET"
    );
}

#[test]
fn metadata_total_and_link_complete_exactly() {
    let second = ISSUE
        .replace("\"number\":7", "\"number\":8")
        .replace("\"id\":501", "\"id\":502");
    let (base, server) = server::sequence_with_headers(vec![
        (REPO.into(), vec![]),
        (
            format!("[{ISSUE}]"),
            vec![
                ("X-Total-Count".into(), "2".into()),
                ("Link".into(), "<{BASE}?page=2>; rel=\"next\"".into()),
            ],
        ),
        (
            format!("[{second}]"),
            vec![
                ("X-Total-Count".into(), "2".into()),
                ("Link".into(), "<{BASE}?page=1>; rel=\"prev\"".into()),
            ],
        ),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let page = provider
        .search_issues(&IssueSearchOptions {
            query: None,
            state: "all".into(),
            page: 1,
            limit: 50,
            include_body: false,
            all: true,
        })
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.total_count, Some(2));
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn repeated_page_fails_before_appending_without_progress() {
    let (base, server) = server::sequence(vec![
        REPO.into(),
        format!("[{ISSUE}]"),
        format!("[{ISSUE}]"),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let error = provider
        .search_issues(&IssueSearchOptions {
            query: None,
            state: "all".into(),
            page: 1,
            limit: 50,
            include_body: false,
            all: true,
        })
        .unwrap_err();
    assert!(
        matches!(
            error,
            crate::providers::api::PhasegentError::Pagination { .. }
        ),
        "{error:?}"
    );
    assert_eq!(
        server.join().unwrap().len(),
        3,
        "the repeat must fail on the second page, not after a partial success"
    );
    let second = ISSUE
        .replace("\"number\":7", "\"number\":8")
        .replace("\"id\":501", "\"id\":502");
    let (base, server) = server::sequence_with_headers(vec![
        (REPO.into(), vec![]),
        (format!("[{ISSUE}]"), vec![]),
        (format!("[{second}]"), vec![]),
        (
            format!("[{ISSUE}]"),
            vec![("Link".into(), "<{BASE}?page=2>; rel=prev".into())],
        ),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let result = provider.search_issues(&IssueSearchOptions {
        query: None,
        state: "all".into(),
        page: 1,
        limit: 50,
        include_body: false,
        all: true,
    });
    assert!(
        matches!(
            result,
            Err(crate::providers::api::PhasegentError::Pagination { .. })
        ),
        "a repeated earlier page must not produce duplicate results: {result:?}"
    );
    assert_eq!(server.join().unwrap().len(), 4);
}

#[test]
fn invalid_or_contradictory_metadata_refuses() {
    let cases = [
        (
            "invalid total",
            format!("[{ISSUE}]"),
            vec![("X-Total-Count".to_owned(), "many".to_owned())],
        ),
        (
            "total above page",
            format!("[{ISSUE}]"),
            vec![("X-Total-Count".to_owned(), "0".to_owned())],
        ),
        (
            "link disagrees",
            format!("[{ISSUE}]"),
            vec![
                ("X-Total-Count".to_owned(), "1".to_owned()),
                ("Link".to_owned(), "<{BASE}?page=2>; rel=next".to_owned()),
            ],
        ),
        (
            "malformed link",
            format!("[{ISSUE}]"),
            vec![("Link".to_owned(), "<{BASE}?page=2>".to_owned())],
        ),
        (
            "unterminated relation quote",
            format!("[{ISSUE}]"),
            vec![("Link".to_owned(), "<{BASE}?page=1>; rel=\"prev".to_owned())],
        ),
        (
            "duplicate relation",
            format!("[{ISSUE}]"),
            vec![(
                "Link".to_owned(),
                "<{BASE}?page=2>; rel=next; rel=prev".to_owned(),
            )],
        ),
        (
            "empty target",
            format!("[{ISSUE}]"),
            vec![("Link".to_owned(), "<>; rel=next".to_owned())],
        ),
        (
            "trailing quoted garbage",
            format!("[{ISSUE}]"),
            vec![(
                "Link".to_owned(),
                "<{BASE}?page=2>; rel=\"next\"junk".to_owned(),
            )],
        ),
        (
            "unknown relation",
            format!("[{ISSUE}]"),
            vec![("Link".to_owned(), "<{BASE}?page=2>; rel=sidebar".to_owned())],
        ),
        (
            "empty before total",
            "[]".to_owned(),
            vec![("X-Total-Count".to_owned(), "1".to_owned())],
        ),
    ];
    for (label, body, headers) in cases {
        let (base, server) =
            server::sequence_with_headers(vec![(REPO.into(), vec![]), (body, headers)]);
        let provider =
            ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture")
                .unwrap();
        let error = provider
            .search_issues(&IssueSearchOptions {
                query: None,
                state: "all".into(),
                page: 1,
                limit: 50,
                include_body: false,
                all: true,
            })
            .unwrap_err();
        assert!(
            matches!(
                error,
                crate::providers::api::PhasegentError::Pagination { .. }
            ),
            "{label}: {error:?}"
        );
        assert_eq!(server.join().unwrap().len(), 2, "{label}");
    }
    let (base, server) = server::sequence_with_headers(vec![
        (REPO.into(), vec![]),
        (
            format!("[{ISSUE}]"),
            vec![("X-Total-Count".into(), "2".into())],
        ),
        ("[]".into(), vec![]),
    ]);
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let result = provider.search_issues(&IssueSearchOptions {
        query: None,
        state: "all".into(),
        page: 1,
        limit: 50,
        include_body: false,
        all: true,
    });
    assert!(
        matches!(
            result,
            Err(crate::providers::api::PhasegentError::Pagination { .. })
        ),
        "lost total must not return a partial list: {result:?}"
    );
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn caller_pagination_keys_refuse_before_http() {
    let (base, server) = server::sequence(Vec::new());
    let provider =
        ForgejoProvider::new(ForgejoConfig::new(base, "owner/repo").unwrap(), "fixture").unwrap();
    let error = provider
        .collect::<serde_json::Value>(&["issues"], &[("page", "2".into())], "issue search")
        .unwrap_err();
    assert!(
        matches!(
            error,
            crate::providers::api::PhasegentError::Pagination { .. }
        ),
        "{error:?}"
    );
    assert!(server.join().unwrap().is_empty());
}
