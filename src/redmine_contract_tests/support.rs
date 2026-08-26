use crate::provider::{RedmineConfig, RedmineProvider};
use crate::redmine_model::{RedmineCurrentUser, RedmineCurrentUserResponse};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

pub(crate) const TEST_API_KEY: &str = "test-redmine-key";

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
    RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap()
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

pub(crate) fn assert_request(request: &str, method: &str, path: &str, body: Option<&str>) {
    assert_request_with_key(request, method, path, body, TEST_API_KEY);
}

pub(crate) fn assert_request_with_key(
    request: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
    api_key: &str,
) {
    assert!(
        request.starts_with(&format!("{method} {path}")),
        "request: {request}"
    );
    let header = format!("x-redmine-api-key: {api_key}");
    assert!(
        request.to_ascii_lowercase().contains(&header),
        "missing Redmine API key header: {request}"
    );
    if let Some(body) = body {
        assert!(
            request.contains(body),
            "request body missing {body}: {request}"
        );
    }
}

pub(crate) fn assert_request_with_bearer(
    request: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
    bearer_key: &str,
) {
    assert!(
        request.starts_with(&format!("{method} {path}")),
        "request: {request}"
    );
    let header = format!("authorization: bearer {bearer_key}");
    assert!(
        request.to_ascii_lowercase().contains(&header),
        "missing bearer key header: {request}"
    );
    // The mirror plugin does not use the role-scoped Redmine API key.
    assert!(
        !request.to_ascii_lowercase().contains("x-redmine-api-key"),
        "mirror plugin request leaked the role-scoped Redmine API key header: {request}"
    );
    if let Some(body) = body {
        assert!(
            request.contains(body),
            "request body missing {body}: {request}"
        );
    }
}

pub(crate) fn issue_response(
    id: u64,
    subject: &str,
    description: &str,
    closed: bool,
    journals: &[(u64, &str)],
) -> String {
    serde_json::json!({
        "issue": {
            "id": id,
            "subject": subject,
            "description": description,
            "status": {"name": if closed {"Closed"} else {"New"}, "is_closed": closed},
            "journals": journals.iter().map(|(journal_id, notes)| serde_json::json!({
                "id": journal_id,
                "notes": notes,
            })).collect::<Vec<_>>(),
        }
    })
    .to_string()
}

pub(crate) fn issue_collection(
    total_count: usize,
    limit: usize,
    issues: &[(u64, &str, bool)],
) -> String {
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "issues": issues.iter().map(|(id, subject, closed)| serde_json::json!({
            "id": id,
            "subject": subject,
            "description": "body",
            "status": {"name": if *closed {"Closed"} else {"New"}, "is_closed": *closed},
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

pub(crate) fn project_collection(
    total_count: usize,
    limit: usize,
    projects: &[(u64, &str, &str)],
) -> String {
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "projects": projects.iter().map(|(id, name, identifier)| serde_json::json!({
            "id": id,
            "name": name,
            "identifier": identifier,
            "description": "description",
            "is_public": false,
            "inherit_members": false,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

pub(crate) fn project_response(id: u64, name: &str, identifier: &str, description: &str) -> String {
    serde_json::json!({
        "project": {
            "id": id,
            "name": name,
            "identifier": identifier,
            "description": description,
            "is_public": false,
            "inherit_members": false,
        }
    })
    .to_string()
}

pub(crate) fn version_collection(versions: &[(u64, &str, &str, Option<&str>)]) -> String {
    version_collection_page(versions.len(), 100, versions)
}

pub(crate) fn version_collection_page(
    total_count: usize,
    limit: usize,
    versions: &[(u64, &str, &str, Option<&str>)],
) -> String {
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "versions": versions.iter().map(|(id, name, status, due_date)| serde_json::json!({
            "id": id,
            "name": name,
            "status": status,
            "due_date": due_date,
            "sharing": "none",
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

pub(crate) fn role_collection(roles: &[(u64, &str)]) -> String {
    role_collection_page(roles.len(), 100, roles)
}

pub(crate) fn role_collection_page(
    total_count: usize,
    limit: usize,
    roles: &[(u64, &str)],
) -> String {
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "roles": roles.iter().map(|(id, name)| serde_json::json!({
            "id": id,
            "name": name,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

pub(crate) fn membership_collection(membership: Option<(u64, u64, &str, Vec<u64>)>) -> String {
    membership_collection_page(usize::from(membership.is_some()), 100, membership)
}

pub(crate) fn membership_collection_page(
    total_count: usize,
    limit: usize,
    membership: Option<(u64, u64, &str, Vec<u64>)>,
) -> String {
    let memberships = membership
        .into_iter()
        .map(|(id, user_id, user_login, role_ids)| {
            serde_json::json!({
                "id": id,
                "user": {"id": user_id, "login": user_login},
                "roles": role_ids
                    .into_iter()
                    .map(|role_id| serde_json::json!({"id": role_id}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "total_count": total_count,
        "limit": limit,
        "memberships": memberships,
    })
    .to_string()
}

pub(crate) fn current_user_response(id: u64, login: &str) -> String {
    serde_json::json!({
        "user": {
            "id": id,
            "login": login,
            "firstname": "Test",
            "lastname": "User",
            "mail": format!("{login}@example.test"),
        }
    })
    .to_string()
}

pub(crate) fn user_from_response(body: &str) -> RedmineCurrentUser {
    let response: RedmineCurrentUserResponse =
        serde_json::from_str(body).expect("current user response should deserialize");
    response.user
}

pub(crate) fn git_mirror_response(
    id: u64,
    project_id: u64,
    identifier: &str,
    status: &str,
    remote_url: Option<&str>,
    local_path: Option<&str>,
    error: Option<&str>,
) -> String {
    serde_json::json!({
        "id": id,
        "project_id": project_id,
        "identifier": identifier,
        "status": status,
        "remote_url": remote_url,
        "local_path": local_path,
        "error": error,
    })
    .to_string()
}

pub(crate) fn time_entry_activities(activities: &[(u64, &str, bool)]) -> String {
    json!({
        "time_entry_activities": activities
            .iter()
            .map(|(id, name, is_default)| json!({"id": id, "name": name, "is_default": is_default}))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

pub(crate) fn time_entry_response(
    id: u64,
    issue: u64,
    activity: u64,
    hours: f64,
    comments: &str,
    spent_on: &str,
) -> String {
    json!({
        "time_entry": {
            "id": id,
            "issue": {"id": issue},
            "activity": {"id": activity, "name": "Automation"},
            "hours": hours,
            "comments": comments,
            "spent_on": spent_on,
        }
    })
    .to_string()
}

pub(crate) fn time_entry_collection(entries: &[(u64, u64, u64, f64, &str, &str)]) -> String {
    let entries = entries
        .iter()
        .map(|(id, issue, activity, hours, comments, spent_on)| {
            json!({
                "id": id,
                "issue": {"id": issue},
                "activity": {"id": activity, "name": "Automation"},
                "hours": hours,
                "comments": comments,
                "spent_on": spent_on,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "total_count": entries.len(),
        "limit": 100,
        "time_entries": entries,
    })
    .to_string()
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
            line.to_ascii_lowercase()
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
        "Unprocessable Entity"
    };
    let headers = format!(
        "HTTP/1.1 {} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.body.len()
    );
    stream.write_all(headers.as_bytes()).unwrap();
    stream.write_all(response.body.as_bytes()).unwrap();
}
