use crate::forgejo::{ForgejoConfig, ForgejoProvider};
use crate::provider::{IssueProvider, ProviderDispatcher, RepoProvider};
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
    let (result, request) = one(
        MockResponse::total(format!("[{}]", issue_json()), "1"),
        |provider| provider.search_issues(Some("needle"), "open"),
    );
    assert_eq!(result.unwrap().len(), 1);
    assert_request(&request, "GET", "/api/v1/repos/owner/repo/issues?", None);
    assert!(request.contains("state=open"));
    assert!(request.contains("q=needle"));
    // The search endpoint must always declare `type=issues` so PRs cannot be
    // surfaced as candidate tracking issues.
    assert!(
        request.contains("type=issues"),
        "expected type=issues in {request}"
    );
}

#[test]
fn issue_search_contract_filters_to_issues_without_query() {
    let (result, request) = one(
        MockResponse::total(format!("[{}]", issue_json()), "1"),
        |provider| provider.search_issues(None, "all"),
    );
    assert_eq!(result.unwrap().len(), 1);
    assert!(request.contains("state=all"));
    assert!(
        request.contains("type=issues"),
        "expected type=issues even without a query string: {request}"
    );
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
    let (base, requests, server) = sequence(vec![
        MockResponse::total(format!("[{}]", issue_json()), "2"),
        MockResponse::ok(r#"[{"id":2,"number":8,"title":"Second","body":"","state":"open"}]"#),
    ]);
    let provider = dispatcher(base);
    let issues = provider.search_issues(None, "all").unwrap();
    assert_eq!(issues.len(), 2);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("page=1"));
    assert!(requests[1].contains("page=2"));
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
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!("[{}]", issue_json())),
        MockResponse::ok(format!("[{}]", issue_json())),
    ]);
    let provider = dispatcher(base);
    let error = provider.search_issues(None, "all").unwrap_err();
    assert_eq!(error.json()["kind"], "pagination");
    assert_eq!(requests.recv().unwrap().len(), 2);
    server.join().unwrap();
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
