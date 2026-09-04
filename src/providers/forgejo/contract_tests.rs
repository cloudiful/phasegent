use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
use crate::providers::{IssueProvider, ProviderDispatcher, RepoProvider};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

struct MockResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl MockResponse {
    fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn total(body: impl Into<String>, total: &str) -> Self {
        Self {
            status: 200,
            headers: vec![("X-Total-Count".to_owned(), total.to_owned())],
            body: body.into(),
        }
    }
}

#[test]
fn issue_get_contract() {
    let (result, request) = one(MockResponse::ok(issue_json()), |provider| {
        provider.get_issue(7)
    });
    let issue = result.unwrap();
    assert_eq!(issue.number, 7);
    assert_request(&request, "GET", "/api/v1/repos/owner/repo/issues/7", None);
}

#[test]
fn issue_search_contract() {
    let options = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "open".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let (result, request) = one(
        MockResponse::total(format!("[{}]", issue_json()), "1"),
        |provider| provider.search_issues(&options),
    );
    let output = result.unwrap();
    assert_eq!(output.items.len(), 1);
    assert_eq!(output.page, 1);
    assert_eq!(output.limit, 50);
    assert_eq!(output.total_count, Some(1));
    assert!(!output.has_more);
    // compact output omits bodies
    assert!(output.items[0].body.is_none());
    assert_request(&request, "GET", "/api/v1/repos/owner/repo/issues?", None);
    assert!(request.contains("state=open"));
    assert!(request.contains("q=needle"));
    assert!(request.contains("limit=50"));
    assert!(request.contains("page=1"));
    // The search endpoint must always declare `type=issues` so PRs cannot be
    // surfaced as candidate tracking issues.
    assert!(
        request.contains("type=issues"),
        "expected type=issues in {request}"
    );
}

#[test]
fn issue_search_contract_filters_to_issues_without_query() {
    // Empty query without --all is rejected at the provider layer; the
    // bounded all-issues mode requires an explicit `all=true`.
    let empty_options = crate::providers::IssueSearchOptions {
        query: None,
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let provider = dispatcher("http://127.0.0.1:1/api/v1".to_owned());
    let error = provider.search_issues(&empty_options).unwrap_err();
    assert_eq!(error.json()["kind"], "config");

    let all_options = crate::providers::IssueSearchOptions {
        query: None,
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: true,
    };
    let (result, request) = one(
        MockResponse::total(format!("[{}]", issue_json()), "1"),
        |provider| provider.search_issues(&all_options),
    );
    assert_eq!(result.unwrap().items.len(), 1);
    assert!(request.contains("state=all"));
    assert!(
        request.contains("type=issues"),
        "expected type=issues even without a query string: {request}"
    );
    // Bounded query-less search must not send an empty q param.
    assert!(!request.contains("q="));
}

#[test]
fn issue_create_update_and_close_contracts() {
    let (result, request) = one(MockResponse::ok(issue_json()), |provider| {
        provider.create_issue("Title", "Body")
    });
    assert_eq!(result.unwrap().title, "Title");
    assert_request(
        &request,
        "POST",
        "/api/v1/repos/owner/repo/issues",
        Some("\"title\":\"Title\""),
    );
    assert!(request.contains("\"body\":\"Body\""));

    let (result, request) = one(MockResponse::ok(issue_json()), |provider| {
        provider.update_body(7, "Updated")
    });
    assert!(result.is_ok());
    assert_request(
        &request,
        "PATCH",
        "/api/v1/repos/owner/repo/issues/7",
        Some("\"body\":\"Updated\""),
    );

    let (result, request) = one(MockResponse::ok(issue_json()), |provider| {
        provider.close_issue(7)
    });
    assert!(result.is_ok());
    assert_request(
        &request,
        "PATCH",
        "/api/v1/repos/owner/repo/issues/7",
        Some("\"state\":\"closed\""),
    );
}

#[test]
fn comment_create_get_and_find_contracts() {
    let (result, request) = one(MockResponse::ok(comment_json()), |provider| {
        provider.create_comment(7, "<!-- marker --> body", "<!-- marker -->")
    });
    let created = result.unwrap();
    assert_eq!(created.id, 42);
    assert_eq!(created.marker.as_deref(), Some("<!-- marker -->"));
    assert_request(
        &request,
        "POST",
        "/api/v1/repos/owner/repo/issues/7/comments",
        Some("marker"),
    );

    let (result, request) = one(
        MockResponse::total(format!("[{}]", comment_json()), "1"),
        |provider| provider.get_comment(7, 42),
    );
    assert_eq!(
        result.unwrap().html_url.as_deref(),
        Some("https://forgejo.example/comment/42")
    );
    assert_request(
        &request,
        "GET",
        "/api/v1/repos/owner/repo/issues/7/comments?",
        None,
    );

    let (result, request) = one(
        MockResponse::total(format!("[{}]", comment_json()), "1"),
        |provider| provider.find_marker(7, "marker"),
    );
    assert_eq!(result.unwrap().marker.as_deref(), Some("marker"));
    assert_request(
        &request,
        "GET",
        "/api/v1/repos/owner/repo/issues/7/comments?",
        None,
    );
}

#[test]
fn personal_repo_create_contract() {
    let response = MockResponse::ok(
        r#"{"name":"new-repo","full_name":"owner/new-repo","owner":{"login":"owner"},"private":true,"clone_url":"https://forgejo.example/owner/new-repo.git","ssh_url":"ssh://git@forgejo.example/owner/new-repo.git","html_url":"https://forgejo.example/owner/new-repo"}"#,
    );
    let (result, request) = one(response, |provider| {
        provider.create_repo("owner/new-repo", true, "description", true)
    });
    let repository = result.unwrap();
    assert_eq!(repository.full_name, "owner/new-repo");
    assert_eq!(repository.owner, "owner");
    assert_eq!(repository.name, "new-repo");
    assert!(repository.private);
    assert_eq!(
        repository.clone_url.as_deref(),
        Some("https://forgejo.example/owner/new-repo.git")
    );
    assert_request(
        &request,
        "POST",
        "/api/v1/user/repos",
        Some("\"name\":\"new-repo\""),
    );
    assert!(request.contains("\"private\":true"));
    assert!(request.contains("\"description\":\"description\""));
    assert!(request.contains("\"auto_init\":true"));
}

#[test]
fn organization_repo_create_contract() {
    let response = MockResponse::ok(
        r#"{"name":"new-repo","full_name":"team/new-repo","owner":{"login":"team"},"private":true,"html_url":"https://forgejo.example/team/new-repo"}"#,
    );
    let (result, request) = one(response, |provider| {
        provider.create_repo("team/new-repo", true, "", false)
    });
    let repository = result.unwrap();
    assert_eq!(repository.full_name, "team/new-repo");
    assert_eq!(repository.owner, "team");
    assert_eq!(repository.name, "new-repo");
    assert!(repository.private);
    assert_request(
        &request,
        "POST",
        "/api/v1/orgs/team/repos",
        Some("\"name\":\"new-repo\""),
    );
    assert!(request.contains("\"private\":true"));
    assert!(request.contains("\"description\":\"\""));
    assert!(request.contains("\"auto_init\":false"));
}

#[test]
fn non_success_response_is_structured() {
    let response = MockResponse {
        status: 403,
        headers: Vec::new(),
        body: r#"{"message":"denied"}"#.to_owned(),
    };
    let (result, request) = one(response, |provider| provider.get_issue(7));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "http");
    assert_eq!(error.json()["status"], 403);
    assert_eq!(error.json()["operation"], "issue get");
    assert_request(&request, "GET", "/api/v1/repos/owner/repo/issues/7", None);
}

#[test]
fn clamped_issue_page_uses_total_count() {
    // Single-page bounded fetch: one request, envelope carries total_count
    // and has_more derived from total_count.
    let options = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 1,
        include_body: false,
        all: false,
    };
    let (base, requests, server) = sequence(vec![MockResponse::total(
        format!("[{}]", issue_json()),
        "2",
    )]);
    let provider = dispatcher(base);
    let result = provider.search_issues(&options).unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.total_count, Some(2));
    assert!(result.has_more);
    assert_eq!(result.page, 1);
    assert_eq!(result.limit, 1);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("page=1"));
    assert!(requests[0].contains("limit=1"));
    server.join().unwrap();
}

#[test]
fn clamped_comment_page_finds_later_marker() {
    let (base, requests, server) = sequence(vec![
        MockResponse::total(r#"[{"id":1,"body":"first"}]"#, "2"),
        MockResponse::ok(
            r#"[{"id":2,"body":"target marker","html_url":"https://forgejo.example/comment/2"}]"#,
        ),
    ]);
    let provider = dispatcher(base);
    let comment = provider.find_marker(7, "target").unwrap();
    assert_eq!(comment.id, 2);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("page=1"));
    assert!(requests[1].contains("page=2"));
    server.join().unwrap();
}

#[test]
fn repeated_non_empty_page_returns_without_looping() {
    // Bounded single-page semantics must not loop; a second identical page
    // is never fetched. The previous multi-page pagination guard is now
    // replaced by the single-page contract: one request only.
    let options = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let (base, requests, server) = sequence(vec![MockResponse::ok(format!(
        "[{}]",
        issue_json()
    ))]);
    let provider = dispatcher(base);
    let result = provider.search_issues(&options).unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(requests.recv().unwrap().len(), 1);
    server.join().unwrap();
}

#[test]
fn issue_search_reports_has_more_from_link_and_compact_truncation() {
    // has_more via X-Total-Count and default compact output without bodies.
    let options_compact = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "open".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    let (result, _request) = one(
        MockResponse::ok(format!("[{}]", issue_json())),
        |provider| provider.search_issues(&options_compact),
    );
    let output = result.unwrap();
    assert!(output.items[0].body.is_none());
    assert!(output.items[0].body_truncated.is_none());

    // Explicit body inclusion is bounded and reports truncation.
    let long_body = "a".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES + 10);
    let issue_with_long_body = format!(
        r#"{{"id":2,"number":9,"title":"Long","body":"{long_body}","state":"open","html_url":"https://forgejo.example/issues/9"}}"#
    );
    let options_body = crate::providers::IssueSearchOptions {
        query: Some("q".to_owned()),
        state: "open".to_owned(),
        page: 1,
        limit: 50,
        include_body: true,
        all: false,
    };
    let (result, _request) = one(
        MockResponse::ok(format!("[{issue_with_long_body}]")),
        |provider| provider.search_issues(&options_body),
    );
    let output = result.unwrap();
    assert!(output.items[0].body.is_some());
    assert_eq!(output.items[0].body_truncated, Some(true));
    assert_eq!(
        output.items[0].body.as_ref().unwrap().len(),
        crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES
    );
}

#[test]
fn issue_search_rejects_invalid_page_limit_and_whitespace_query() {
    let base = dispatcher("http://127.0.0.1:1/api/v1".to_owned());
    let bad_page = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "all".to_owned(),
        page: 0,
        limit: 50,
        include_body: false,
        all: false,
    };
    assert_eq!(base.search_issues(&bad_page).unwrap_err().json()["kind"], "config");
    let bad_limit = crate::providers::IssueSearchOptions {
        query: Some("needle".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 101,
        include_body: false,
        all: false,
    };
    assert_eq!(base.search_issues(&bad_limit).unwrap_err().json()["kind"], "config");
    let whitespace = crate::providers::IssueSearchOptions {
        query: Some("   ".to_owned()),
        state: "all".to_owned(),
        page: 1,
        limit: 50,
        include_body: false,
        all: false,
    };
    assert_eq!(
        base.search_issues(&whitespace).unwrap_err().json()["kind"],
        "config"
    );
}

fn issue_json() -> &'static str {
    r#"{"id":1,"number":7,"title":"Title","body":"Body","state":"open","html_url":"https://forgejo.example/issues/7"}"#
}

fn comment_json() -> &'static str {
    r#"{"id":42,"body":"<!-- marker --> body","html_url":"https://forgejo.example/comment/42"}"#
}

fn dispatcher(base: String) -> ProviderDispatcher {
    ProviderDispatcher::Forgejo(
        ForgejoProvider::new(
            ForgejoConfig::new(base, "owner", "repo"),
            "token".to_owned(),
        )
        .unwrap(),
    )
}

fn one<T>(response: MockResponse, operation: impl FnOnce(&ProviderDispatcher) -> T) -> (T, String) {
    let (base, requests, server) = sequence(vec![response]);
    let provider = dispatcher(base);
    let result = operation(&provider);
    let request = requests.recv().unwrap().remove(0);
    server.join().unwrap();
    (result, request)
}

fn assert_request(request: &str, method: &str, path: &str, body: Option<&str>) {
    assert!(
        request.starts_with(&format!("{method} {path}")),
        "request: {request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer token")
    );
    if let Some(body) = body {
        assert!(
            request.contains(body),
            "request body missing {body}: {request}"
        );
    }
}

fn sequence(responses: Vec<MockResponse>) -> (String, Receiver<Vec<String>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            requests.push(request);
            write_response(&mut stream, response);
        }
        sender.send(requests).unwrap();
    });
    (format!("http://{address}/api/v1"), receiver, server)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let size = stream.read(&mut chunk).unwrap();
        if size == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..size]);
        if request_complete(&bytes) {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn request_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    bytes.len() >= header_end + 4 + content_length
}

fn write_response(stream: &mut TcpStream, response: MockResponse) {
    let status_text = if response.status == 200 {
        "OK"
    } else {
        "Forbidden"
    };
    let mut headers = format!(
        "HTTP/1.1 {} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.body.len()
    );
    for (name, value) in response.headers {
        headers.push_str(&format!("{name}: {value}\r\n"));
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes()).unwrap();
    stream.write_all(response.body.as_bytes()).unwrap();
}
