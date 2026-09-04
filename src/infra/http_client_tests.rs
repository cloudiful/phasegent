use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::infra::http_client;
use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
use crate::providers::gitlab::http::GitlabHttp;
use crate::providers::redmine::http::{RedmineGitMirrorHttp, RedmineHttp};

// Small static gzip fixtures generated via `python -m gzip` for
// `{"id":1,"title":"gzipped"}` and `plain text log line 1\nline 2\n`.
const GZIP_JSON: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x35, 0xba, 0x90, 0x6a, 0x02, 0xff, 0xab, 0x56, 0xca, 0x4c, 0x51, 0xb2,
    0x32, 0xd4, 0x51, 0x2a, 0xc9, 0x2c, 0xc9, 0x49, 0x55, 0xb2, 0x52, 0x4a, 0xaf, 0xca, 0x2c, 0x28,
    0x48, 0x4d, 0x51, 0xaa, 0x05, 0x00, 0xb4, 0xef, 0xd8, 0x63, 0x1a, 0x00, 0x00, 0x00,
];
const GZIP_TEXT: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x35, 0xba, 0x90, 0x6a, 0x02, 0xff, 0x2b, 0xc8, 0x49, 0xcc, 0xcc, 0x53,
    0x28, 0x49, 0xad, 0x28, 0x51, 0xc8, 0xc9, 0x4f, 0x57, 0xc8, 0xc9, 0xcc, 0x4b, 0x55, 0x30, 0xe4,
    0x02, 0x53, 0x46, 0x5c, 0x00, 0x94, 0x6e, 0xee, 0xb5, 0x1d, 0x00, 0x00, 0x00,
];

struct MockResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl MockResponse {
    fn json(body: &str) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn status(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn with_header(mut self, key: &str, value: &str) -> Self {
        self.headers.push((key.to_owned(), value.to_owned()));
        self
    }

    fn gzip_json() -> Self {
        Self {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("Content-Encoding".into(), "gzip".into()),
            ],
            body: GZIP_JSON.to_vec(),
        }
    }

    fn gzip_text() -> Self {
        Self {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "text/plain".into()),
                ("Content-Encoding".into(), "gzip".into()),
            ],
            body: GZIP_TEXT.to_vec(),
        }
    }
}

fn sequence(responses: Vec<MockResponse>) -> (String, Receiver<Vec<String>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let req = read_request(&mut stream);
            requests.push(req);
            write_response(&mut stream, response);
        }
        let _ = sender.send(requests);
    });
    (format!("http://{addr}"), receiver, handle)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 4096];
    let header_end;
    loop {
        let n = stream.read(&mut tmp).unwrap();
        if n == 0 {
            header_end = buf.len();
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = end + 4;
            break;
        }
        if buf.len() > 16 * 1024 {
            header_end = buf.len();
            break;
        }
    }
    let header_text = String::from_utf8_lossy(&buf[..header_end]);
    let is_chunked = header_text
        .lines()
        .any(|line| line.eq_ignore_ascii_case("Transfer-Encoding: chunked"));
    let content_length = header_text
        .lines()
        .find_map(|line| {
            line.strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    while is_chunked && !buf[header_end..].windows(5).any(|w| w == b"0\r\n\r\n") {
        let n = stream.read(&mut tmp).unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    while !is_chunked && buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp).unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn write_response(stream: &mut TcpStream, resp: MockResponse) {
    let status_text = match resp.status {
        200 => "OK",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        404 => "Not Found",
        400 => "Bad Request",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        _ => "Error",
    };
    let mut header_block = format!(
        "HTTP/1.1 {} {status_text}\r\nContent-Length: {}\r\nConnection: close\r\n",
        resp.status,
        resp.body.len()
    );
    for (k, v) in resp.headers {
        header_block.push_str(&format!("{k}: {v}\r\n"));
    }
    header_block.push_str("\r\n");
    stream.write_all(header_block.as_bytes()).unwrap();
    stream.write_all(&resp.body).unwrap();
}

fn stall_server(delay: Duration) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = read_request(&mut stream);
            thread::sleep(delay);
            // Drop without response to trigger timeout on client side.
        }
    });
    (format!("http://{addr}"), handle)
}

#[test]
fn shared_factory_constants_are_authoritative() {
    assert_eq!(http_client::CONNECT_TIMEOUT, Duration::from_secs(10));
    assert_eq!(http_client::REQUEST_TIMEOUT, Duration::from_secs(30));
    assert!(http_client::is_retryable_status(
        reqwest::StatusCode::from_u16(429).unwrap()
    ));
    assert!(http_client::is_retryable_status(
        reqwest::StatusCode::from_u16(503).unwrap()
    ));
    assert!(!http_client::is_retryable_status(
        reqwest::StatusCode::from_u16(400).unwrap()
    ));
    // All four construction sites build via the shared factory without panic.
    let _ = ForgejoProvider::new(
        ForgejoConfig::new("http://example.com/api/v1", "owner", "repo"),
        "token".to_owned(),
    )
    .unwrap();
    let _ = RedmineHttp::new("http://example.com".into(), "key".into()).unwrap();
    let _ = RedmineGitMirrorHttp::new("http://example.com".into(), "bearer".into()).unwrap();
    let _ = GitlabHttp::new("http://example.com/api/v4".into(), "glpat-test".into()).unwrap();
}

#[test]
fn timeout_with_short_deadline_is_bounded() {
    // Peer stalls longer than the request timeout; client must error boundedly,
    // not hang. Use a short test-only timeout so the test stays fast.
    let (base, handle) = stall_server(Duration::from_millis(800));
    let client = http_client::build_client_with_timeouts(
        Duration::from_millis(100),
        Duration::from_millis(200),
    )
    .unwrap();
    let start = Instant::now();
    let result =
        http_client::fetch_with_retry(client.get(format!("{base}/stall")), "stall test", |m| {
            m.to_owned()
        });
    let elapsed = start.elapsed();
    assert!(result.is_err(), "stall must produce a bounded error");
    // 3 attempts * 200ms timeout + 100ms/200ms backoff ≈ < 1.2s
    assert!(
        elapsed < Duration::from_secs(3),
        "timeout must be bounded, elapsed={elapsed:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(150),
        "must have waited for timeout, elapsed={elapsed:?}"
    );
    let _ = handle.join();
}

#[test]
fn gzip_json_and_text_are_transparently_decoded() {
    // JSON via Forgejo GET with gzip.
    let (base, requests, server) = sequence(vec![MockResponse::gzip_json()]);
    let provider = ForgejoProvider::new(
        ForgejoConfig::new(base.clone(), "owner", "repo"),
        "token".into(),
    )
    .unwrap();
    let issue: serde_json::Value = provider
        .get(
            &format!("{base}/repos/owner/repo/issues/1"),
            &[],
            "gzip json",
        )
        .unwrap();
    assert_eq!(issue["title"], "gzipped");
    let req = requests.recv().unwrap().remove(0);
    assert!(
        req.to_ascii_lowercase().contains("accept-encoding"),
        "gzip feature must negotiate Accept-Encoding, got: {req}"
    );
    server.join().unwrap();

    // Text via the shared retry/decompression helper with gzip.
    let (base, requests, server) = sequence(vec![MockResponse::gzip_text()]);
    let client = http_client::build_client().unwrap();
    let (_status, _headers, text) = http_client::fetch_with_retry(
        client.get(format!("{base}/trace")),
        "ci job logs",
        |message| message.to_owned(),
    )
    .unwrap();
    assert!(text.contains("plain text log line 1"));
    assert!(text.contains("line 2"));
    let req = requests.recv().unwrap().remove(0);
    assert!(
        req.to_ascii_lowercase().contains("accept-encoding"),
        "gzip feature must negotiate Accept-Encoding, got: {req}"
    );
    server.join().unwrap();
}

#[path = "http_client_retry_tests.rs"]
mod retry_tests;
