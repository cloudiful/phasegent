#![allow(unused_imports)]
use crate::infra::storage::test_support::EnvGuard;
use crate::providers::redmine::model::{RedmineCurrentUser, RedmineCurrentUserResponse};
use crate::providers::{RedmineConfig, RedmineProvider};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(crate) struct MockResponse {
    status: u16,
    body: String,
}

impl MockResponse {
    pub(crate) fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
        }
    }

    pub(crate) fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }

    pub(crate) fn error(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }
}

pub(crate) fn provider(base: String) -> RedmineProvider {
    RedmineProvider::new(
        RedmineConfig::new(base, "42", 37),
        super::TEST_API_KEY.to_owned(),
    )
    .unwrap()
}

pub(crate) fn one<T>(
    response: MockResponse,
    operation: impl FnOnce(&RedmineProvider) -> T,
) -> (T, String) {
    let (base, requests, server) = sequence(vec![response]);
    let redmine = provider(base);
    let result = operation(&redmine);
    let request = requests.recv().unwrap().remove(0);
    server.join().unwrap();
    (result, request)
}

pub(crate) fn sequence(
    responses: Vec<MockResponse>,
) -> (String, Receiver<Vec<String>>, JoinHandle<()>) {
    let (base, requests, _optional, server) = sequence_with_optional(responses, None);
    (base, requests, server)
}

pub(crate) type OptionalSequence = (
    String,
    Receiver<Vec<String>>,
    Receiver<Vec<String>>,
    JoinHandle<()>,
);

/// Serve the required responses in order while recognising at most one exact
/// optional request before each required one. The optional request is
/// recorded on its own channel and never consumes a required response.
pub(crate) fn sequence_with_optional(
    responses: Vec<MockResponse>,
    optional: Option<(&'static str, MockResponse)>,
) -> OptionalSequence {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let (optional_sender, optional_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut optional = optional;
        let mut requests = Vec::new();
        let mut optional_requests = Vec::new();
        for response in responses {
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                let is_optional = optional.as_ref().is_some_and(|(method_path, _)| {
                    request
                        .strip_prefix(*method_path)
                        .is_some_and(|rest| rest.starts_with(" HTTP/1.1\r\n"))
                });
                if is_optional {
                    let (_, optional_response) = optional.take().unwrap();
                    optional_requests.push(request);
                    write_response(&mut stream, optional_response);
                    continue;
                }
                requests.push(request);
                write_response(&mut stream, response);
                break;
            }
        }
        sender.send(requests).unwrap();
        // The plain `sequence` wrapper drops this optional channel; the
        // server must still exit cleanly when no optional request occurred.
        let _ = optional_sender.send(optional_requests);
    });
    (
        format!("http://{address}"),
        receiver,
        optional_receiver,
        server,
    )
}

pub(crate) fn sequence_raw(
    responses: Vec<MockResponse>,
) -> (String, Receiver<Vec<Vec<u8>>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request_raw(&mut stream);
            requests.push(request);
            write_response(&mut stream, response);
        }
        sender.send(requests).unwrap();
    });
    (format!("http://{address}"), receiver, server)
}

fn read_request(stream: &mut TcpStream) -> String {
    String::from_utf8_lossy(&read_request_raw(stream)).into_owned()
}

fn read_request_raw(stream: &mut TcpStream) -> Vec<u8> {
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
    bytes
}

fn request_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
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
        422 => "Unprocessable Entity",
        _ => "OK",
    };
    let headers = format!(
        "HTTP/1.1 {} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.body.len()
    );
    stream.write_all(headers.as_bytes()).unwrap();
    stream.write_all(response.body.as_bytes()).unwrap();
}
