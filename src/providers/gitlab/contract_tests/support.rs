//! Shared test support for GitLab contract tests.

use crate::providers::ProviderDispatcher;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::GitlabProvider;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(super) const TEST_TOKEN: &str = "glpat-test-token-do-not-leak";

pub(super) struct MockResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl MockResponse {
    pub(super) fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub(super) fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub(super) fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

pub(super) fn dispatcher(base: String) -> ProviderDispatcher {
    ProviderDispatcher::Gitlab(
        GitlabProvider::new(
            GitlabConfig::new(format!("{base}/api/v4"), 42),
            TEST_TOKEN.to_owned(),
        )
        .unwrap(),
    )
}

pub(super) fn provider(base: String) -> GitlabProvider {
    GitlabProvider::new(
        GitlabConfig::new(format!("{base}/api/v4"), 42),
        TEST_TOKEN.to_owned(),
    )
    .unwrap()
}

pub(super) fn one<T>(
    response: MockResponse,
    operation: impl FnOnce(&GitlabProvider) -> T,
) -> (T, String) {
    let (base, requests, server) = sequence(vec![response]);
    let provider = provider(base);
    let result = operation(&provider);
    let request = requests.recv().unwrap().remove(0);
    server.join().unwrap();
    (result, request)
}

#[allow(dead_code)]
pub(super) fn one_dispatcher<T>(
    response: MockResponse,
    operation: impl FnOnce(&ProviderDispatcher) -> T,
) -> (T, String) {
    let (base, requests, server) = sequence(vec![response]);
    let dispatcher = dispatcher(base);
    let result = operation(&dispatcher);
    let request = requests.recv().unwrap().remove(0);
    server.join().unwrap();
    (result, request)
}

pub(super) fn zero_request<T>(operation: impl FnOnce(&GitlabProvider) -> T) -> T {
    let provider = provider("http://127.0.0.1:1".to_owned());
    operation(&provider)
}

pub(super) fn zero_request_provider() -> GitlabProvider {
    provider("http://127.0.0.1:1".to_owned())
}

pub(super) fn sequence(
    responses: Vec<MockResponse>,
) -> (String, Receiver<Vec<String>>, JoinHandle<()>) {
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
    (format!("http://{address}"), receiver, server)
}

pub(super) fn assert_request(request: &str, method: &str, path: &str, body: Option<&str>) {
    assert!(
        request.starts_with(&format!("{method} {path}")),
        "request: {request}"
    );
    let header = format!("private-token: {TEST_TOKEN}");
    assert!(
        request.to_ascii_lowercase().contains(&header),
        "missing PRIVATE-TOKEN header: {request}"
    );
    assert!(
        !request.to_ascii_lowercase().contains("authorization:"),
        "GitLab request leaked an Authorization header: {request}"
    );
    if let Some(body) = body {
        assert!(
            request.contains(body),
            "request body missing {body}: {request}"
        );
    }
}

pub(super) fn issue_payload(iid: u64, title: &str, state: &str, labels: &[&str]) -> String {
    serde_json::json!({
        "id": iid + 100,
        "iid": iid,
        "title": title,
        "description": format!("body-of-{iid}"),
        "state": state,
        "labels": labels,
        "web_url": format!("https://gitlab.example/group/project/-/issues/{iid}"),
    })
    .to_string()
}

pub(super) fn note_payload(id: u64, body: &str) -> String {
    serde_json::json!({
        "id": id,
        "body": body,
        "system": false,
        "confidential": false,
    })
    .to_string()
}

pub(super) fn label_payload(id: u64, name: &str) -> String {
    serde_json::json!({
        "id": id,
        "name": name,
        "color": "#cccccc",
        "description": null,
    })
    .to_string()
}

pub(super) fn project_payload(
    id: u64,
    path: &str,
    namespace_path: &str,
    visibility: &str,
) -> String {
    serde_json::json!({
        "id": id,
        "name": path,
        "path": path,
        "path_with_namespace": format!("{namespace_path}/{path}"),
        "web_url": format!("https://gitlab.example/{namespace_path}/{path}"),
        "default_branch": "main",
        "visibility": visibility,
        "description": null,
        "namespace": {
            "id": 1,
            "path": namespace_path,
            "full_path": namespace_path,
            "kind": "group",
        },
        "http_url_to_repo": format!(
            "https://gitlab.example/{namespace_path}/{path}.git"
        ),
        "ssh_url_to_repo": format!(
            "ssh://git@gitlab.example/{namespace_path}/{path}.git"
        ),
    })
    .to_string()
}

pub(super) fn user_payload(id: u64) -> String {
    serde_json::json!({"id": id, "username": "owner"}).to_string()
}

pub(super) fn open_temp_storage() -> crate::infra::storage::Storage {
    let home = std::env::temp_dir().join(format!(
        "phasegent-timer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    crate::infra::storage::Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap()
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
    let status_text = match response.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        403 => "Forbidden",
        404 => "Not Found",
        422 => "Unprocessable Entity",
        _ => "Error",
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
