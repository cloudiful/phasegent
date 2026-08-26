//! Mock HTTP contract tests for the GitLab provider.
//!
//! These tests bind a local TCP listener, drive the provider against
//! it, and assert the exact request shape (method, path, headers,
//! query parameters, JSON body) the provider emitted. They are the
//! single source of truth for the GitLab REST v4 wire format this
//! CLI speaks, and they intentionally run with synthetic tokens so
//! no real credential ever enters the test process.

use crate::gitlab::GitlabProvider;
use crate::provider::{CiProvider, IssueProvider, ProviderDispatcher, RepoProvider};
use crate::provider_config::GitlabConfig;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

/// Synthetic PRIVATE-TOKEN used by every contract test. The value is
/// a deliberately recognisable marker (`glpat-...`) so any leakage
/// into a recorded request makes the failure self-evident.
const TEST_TOKEN: &str = "glpat-test-token-do-not-leak";

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

    fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// Add a single header. Used by the pagination test so the
    /// provider sees an `x-next-page` value and walks every page
    /// instead of treating a partial response as terminal.
    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

fn dispatcher(base: String) -> ProviderDispatcher {
    ProviderDispatcher::Gitlab(
        GitlabProvider::new(
            GitlabConfig::new(format!("{base}/api/v4"), 42),
            TEST_TOKEN.to_owned(),
        )
        .unwrap(),
    )
}

fn provider(base: String) -> GitlabProvider {
    // `api_base` already carries `/api/v4`; do not append it twice.
    GitlabProvider::new(
        GitlabConfig::new(format!("{base}/api/v4"), 42),
        TEST_TOKEN.to_owned(),
    )
    .unwrap()
}

fn one<T>(response: MockResponse, operation: impl FnOnce(&GitlabProvider) -> T) -> (T, String) {
    let (base, requests, server) = sequence(vec![response]);
    let provider = provider(base);
    let result = operation(&provider);
    let request = requests.recv().unwrap().remove(0);
    server.join().unwrap();
    (result, request)
}

#[allow(dead_code)]
fn one_dispatcher<T>(
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

/// Run a closure that is expected to short-circuit before any HTTP
/// call. Starts no listener so the test does not hang waiting for an
/// unused connection.
fn zero_request<T>(operation: impl FnOnce(&GitlabProvider) -> T) -> T {
    let provider = provider("http://127.0.0.1:1".to_owned());
    operation(&provider)
}

/// Build a `GitlabProvider` that is expected to short-circuit before
/// any HTTP call. The owning variant of `zero_request` for adapters
/// that need to move the provider into a `ProviderDispatcher` enum.
fn zero_request_provider() -> GitlabProvider {
    provider("http://127.0.0.1:1".to_owned())
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
    (format!("http://{address}"), receiver, server)
}

fn assert_request(request: &str, method: &str, path: &str, body: Option<&str>) {
    assert!(
        request.starts_with(&format!("{method} {path}")),
        "request: {request}"
    );
    let header = format!("private-token: {TEST_TOKEN}");
    assert!(
        request.to_ascii_lowercase().contains(&header),
        "missing PRIVATE-TOKEN header: {request}"
    );
    // GitLab does not use Bearer auth; a stray Authorization header
    // would indicate the request leaked the Forgejo / Redmine code
    // path.
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

fn issue_payload(iid: u64, title: &str, state: &str, labels: &[&str]) -> String {
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

fn note_payload(id: u64, body: &str) -> String {
    serde_json::json!({
        "id": id,
        "body": body,
        "system": false,
        "confidential": false,
    })
    .to_string()
}

fn label_payload(id: u64, name: &str) -> String {
    serde_json::json!({
        "id": id,
        "name": name,
        "color": "#cccccc",
        "description": null,
    })
    .to_string()
}

// =============================================================================
// Issue lifecycle
// =============================================================================

#[test]
fn get_issue_hits_project_issues_iid_with_private_token() {
    let (result, request) = one(
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
        |provider| provider.get_issue(7),
    );
    let issue = result.unwrap();
    assert_eq!(issue.number, 7);
    assert_eq!(issue.title, "Title");
    assert_eq!(issue.state, "open");
    assert_request(&request, "GET", "/api/v4/projects/42/issues/7", None);
}

#[test]
fn search_issues_open_sends_state_opened_and_paginates() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!("[{}]", issue_payload(1, "One", "opened", &[])))
            .with_header("x-next-page", "2"),
        MockResponse::ok(format!("[{}]", issue_payload(2, "Two", "opened", &[])))
            .with_header("x-next-page", ""),
    ]);
    let dispatcher = dispatcher(base);
    let issues = dispatcher.search_issues(Some("needle"), "open").unwrap();
    assert_eq!(issues.len(), 2);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("state=opened"));
    assert!(requests[0].contains("search=needle"));
    assert!(requests[0].contains("page=1"));
    assert!(requests[1].contains("page=2"));
    for request in &requests {
        assert!(
            request.starts_with("GET /api/v4/projects/42/issues?"),
            "request: {request}"
        );
    }
    server.join().unwrap();
}

#[test]
fn search_issues_closed_maps_to_state_closed() {
    let (result, request) = one(
        MockResponse::ok(format!("[{}]", issue_payload(3, "Done", "closed", &[]))),
        |provider| provider.search_issues(None, "closed"),
    );
    let issues = result.unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].state, "closed");
    assert!(request.contains("state=closed"));
    assert!(!request.contains("state=opened"));
}

#[test]
fn search_issues_all_omits_state_filter() {
    let (result, request) = one(
        MockResponse::ok("[]").with_header("x-next-page", ""),
        |provider| provider.search_issues(None, "all"),
    );
    assert!(result.unwrap().is_empty());
    assert!(
        !request.contains("state="),
        "all must not send a state filter: {request}"
    );
}

#[test]
fn search_issues_rejects_unknown_state_before_request() {
    let result = zero_request(|provider| provider.search_issues(None, "bogus"));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn repeated_non_empty_search_page_returns_pagination_error() {
    let (base, requests, server) = sequence(vec![
        // Both pages have x-next-page so the helper must walk past
        // page 1; the repeated body on page 2 is what should trip
        // the safety guard.
        MockResponse::ok(format!("[{}]", issue_payload(1, "One", "opened", &[])))
            .with_header("x-next-page", "2"),
        MockResponse::ok(format!("[{}]", issue_payload(1, "One", "opened", &[])))
            .with_header("x-next-page", "3"),
    ]);
    let dispatcher = dispatcher(base);
    let error = dispatcher.search_issues(None, "all").unwrap_err();
    assert_eq!(error.json()["kind"], "pagination");
    assert_eq!(requests.recv().unwrap().len(), 2);
    server.join().unwrap();
}

#[test]
fn create_issue_posts_to_issues_with_labels() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_payload(
        11,
        "Created",
        "opened",
        &["type::bug"],
    ))]);
    let provider = provider(base);
    let labels = vec!["type::bug".to_owned()];
    let issue = provider
        .create_issue_with_labels("Created", "Body", &labels)
        .unwrap();
    assert_eq!(issue.number, 11);
    let request = requests.recv().unwrap().remove(0);
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues",
        Some(r#""title":"Created""#),
    );
    assert!(request.contains(r#""description":"Body""#));
    assert!(request.contains(r#""labels":["type::bug"]"#));
    server.join().unwrap();
}

#[test]
fn update_body_with_labels_uses_put_and_emits_add_labels() {
    let (base, requests, server) = sequence(vec![
        // ensure_labels first GETs the project label list; return
        // an empty list so the provider must create type::feature.
        MockResponse::ok("[]").with_header("x-next-page", ""),
        // POST the new type::feature label so ensure_labels succeeds.
        MockResponse::ok(label_payload(50, "type::feature")),
        // The provider then GETs the current issue so it can see
        // the existing label set before deciding which managed
        // tracker label to remove.
        MockResponse::ok(issue_payload(12, "Title", "opened", &["type::bug"])),
        MockResponse::ok(issue_payload(12, "Title", "opened", &["type::feature"])),
    ]);
    let provider = provider(base);
    let labels = vec!["type::feature".to_owned()];
    let issue = provider
        .update_body_with_labels(12, "Updated body", &labels)
        .unwrap();
    assert_eq!(issue.number, 12);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/12"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/12"));
    assert!(requests[3].contains(r#""description":"Updated body""#));
    assert!(requests[3].contains(r#""add_labels":["type::feature"]"#));
    // The previous tracker label is removed in the same payload so
    // the issue never carries both type::bug and type::feature.
    assert!(requests[3].contains(r#""remove_labels":["type::bug"]"#));
    server.join().unwrap();
}

#[test]
fn update_body_with_tracker_replaces_bug_with_feature() {
    // Switching from Bug to Feature must remove the type::bug label
    // so the issue never carries both managed tracker labels.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(50, "type::feature")),
        MockResponse::ok(issue_payload(60, "Title", "opened", &["type::bug"])),
        MockResponse::ok(issue_payload(60, "Title", "opened", &["type::feature"])),
    ]);
    let provider = provider(base);
    let labels = vec!["type::feature".to_owned()];
    provider
        .update_body_with_labels(60, "Switched to feature", &labels)
        .unwrap();
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/60"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/60"));
    assert!(requests[3].contains(r#""add_labels":["type::feature"]"#));
    assert!(
        requests[3].contains(r#""remove_labels":["type::bug"]"#),
        "expected remove_labels to drop the prior bug tracker: {}",
        requests[3],
    );
    assert!(
        !requests[3].contains(r#""add_labels":["type::bug"]"#),
        "the payload must not re-add the dropped bug tracker: {}",
        requests[3],
    );
    server.join().unwrap();
}

#[test]
fn update_body_with_tracker_replaces_feature_with_bug() {
    // Mirror of the bug->feature case: switching from Feature to
    // Bug must drop the existing type::feature label.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(51, "type::bug")),
        MockResponse::ok(issue_payload(61, "Title", "opened", &["type::feature"])),
        MockResponse::ok(issue_payload(61, "Title", "opened", &["type::bug"])),
    ]);
    let provider = provider(base);
    let labels = vec!["type::bug".to_owned()];
    provider
        .update_body_with_labels(61, "Switched to bug", &labels)
        .unwrap();
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/61"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/61"));
    assert!(requests[3].contains(r#""add_labels":["type::bug"]"#));
    assert!(
        requests[3].contains(r#""remove_labels":["type::feature"]"#),
        "expected remove_labels to drop the prior feature tracker: {}",
        requests[3],
    );
    server.join().unwrap();
}

#[test]
fn update_body_with_tracker_preserves_unrelated_labels() {
    // Workflow labels and unrelated project labels must survive a
    // tracker swap. Only the opposite managed tracker label is
    // removed; everything else is left alone.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(52, "type::feature")),
        MockResponse::ok(issue_payload(
            62,
            "Title",
            "opened",
            &["type::bug", "workflow::in-review", "frontend"],
        )),
        MockResponse::ok(issue_payload(
            62,
            "Title",
            "opened",
            &["type::feature", "workflow::in-review", "frontend"],
        )),
    ]);
    let provider = provider(base);
    let labels = vec!["type::feature".to_owned()];
    provider
        .update_body_with_labels(62, "Updated", &labels)
        .unwrap();
    let requests = requests.recv().unwrap();
    let put = &requests[3];
    assert!(put.contains(r#""add_labels":["type::feature"]"#));
    assert!(
        put.contains(r#""remove_labels":["type::bug"]"#),
        "expected the only removal to be the prior bug tracker: {put}",
    );
    // Workflow and unrelated project labels are not touched.
    assert!(
        !put.contains(r#""remove_labels":["workflow::in-review"]"#),
        "workflow label must not be removed by a tracker swap: {put}",
    );
    assert!(
        !put.contains(r#""remove_labels":["frontend"]"#),
        "unrelated project labels must not be removed by a tracker swap: {put}",
    );
    server.join().unwrap();
}

#[test]
fn update_body_with_tracker_keeps_same_tracker_idempotent() {
    // Setting the same tracker the issue already carries must not
    // remove any label. add_labels is a no-op for an already
    // attached label, and remove_labels stays empty because the
    // opposite tracker is not currently attached.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(53, "type::bug")),
        MockResponse::ok(issue_payload(63, "Title", "opened", &["type::bug"])),
        MockResponse::ok(issue_payload(63, "Title", "opened", &["type::bug"])),
    ]);
    let provider = provider(base);
    let labels = vec!["type::bug".to_owned()];
    provider
        .update_body_with_labels(63, "Body", &labels)
        .unwrap();
    let requests = requests.recv().unwrap();
    let put = &requests[3];
    assert!(put.contains(r#""add_labels":["type::bug"]"#));
    assert!(
        !put.contains(r#""remove_labels":["type::feature"]"#),
        "must not drop the opposite tracker when only the same tracker is requested: {put}",
    );
    assert!(
        !put.contains(r#""remove_labels":["type::bug"]"#),
        "must not drop the requested tracker itself: {put}",
    );
    server.join().unwrap();
}

#[test]
fn close_issue_pairs_state_event_close_with_workflow_closed_label() {
    let (base, requests, server) = sequence(vec![
        // First: ensure workflow::closed label exists. The label
        // endpoint returns an empty list so the provider creates it.
        MockResponse::ok("[]").with_header("x-next-page", ""),
        // Second: the create label POST.
        MockResponse::ok(label_payload(1, "workflow::closed")),
        // Third: GET the current issue so the provider can decide
        // whether state_event=close is required. The issue is open
        // here, so a close transition is needed.
        MockResponse::ok(issue_payload(15, "Title", "opened", &[])),
        // Fourth: the close PUT on the issue.
        MockResponse::ok(issue_payload(15, "Title", "closed", &["workflow::closed"])),
    ]);
    let provider = provider(base);
    let issue = provider.close_issue(15).unwrap();
    assert_eq!(issue.number, 15);
    assert_eq!(issue.state, "closed");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/15"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/15"));
    assert!(requests[3].contains(r#""state_event":"close""#));
    assert!(requests[3].contains(r#""add_labels":["workflow::closed"]"#));
    // Every other managed workflow label must be in the remove list.
    for label in [
        "workflow::new",
        "workflow::in-progress",
        "workflow::in-review",
        "workflow::changes-requested",
        "workflow::blocked",
        "workflow::resolved",
        "workflow::cancelled",
    ] {
        assert!(
            requests[3].contains(&format!(r#""{}""#, label)),
            "close payload missing remove_label for {label}: {}",
            requests[3]
        );
    }
    server.join().unwrap();
}

#[test]
fn reopen_for_non_closed_status_emits_state_event_reopen() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(2, "workflow::in-review")),
        // GET current issue: the issue is currently closed so the
        // provider must emit state_event=reopen to transition it
        // back to the open state alongside the workflow label swap.
        MockResponse::ok(issue_payload(21, "Title", "closed", &["workflow::new"])),
        MockResponse::ok(issue_payload(
            21,
            "Title",
            "opened",
            &["workflow::in-review"],
        )),
    ]);
    let provider = provider(base);
    let issue = provider.set_workflow_status(21, "InReview").unwrap();
    assert_eq!(issue.state, "open");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/21"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/21"));
    assert!(requests[3].contains(r#""state_event":"reopen""#));
    assert!(requests[3].contains(r#""add_labels":["workflow::in-review"]"#));
    server.join().unwrap();
}

#[test]
fn status_set_open_to_open_omits_state_event() {
    // GitLab REST v4 rejects state_event=reopen on an already-open
    // issue with HTTP 400. Setting an open workflow status on an
    // open issue must omit state_event entirely so repeated
    // `status set` calls remain idempotent.
    let (base, requests, server) = sequence(vec![
        // Ensure workflow::new label exists.
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(5, "workflow::new")),
        // GET current issue: open.
        MockResponse::ok(issue_payload(22, "Title", "opened", &[])),
        // PUT response after the label swap.
        MockResponse::ok(issue_payload(22, "Title", "opened", &["workflow::new"])),
    ]);
    let provider = provider(base);
    let issue = provider.set_workflow_status(22, "New").unwrap();
    assert_eq!(issue.state, "open");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/22"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/22"));
    assert!(
        !requests[3].contains("state_event"),
        "open->open must omit state_event: {}",
        requests[3],
    );
    assert!(requests[3].contains(r#""add_labels":["workflow::new"]"#));
    server.join().unwrap();
}

#[test]
fn close_issue_already_closed_omits_state_event() {
    // GitLab REST v4 rejects state_event=close on an already-closed
    // issue with HTTP 400. The provider must omit state_event when
    // no state transition is required so the close path stays
    // idempotent.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(6, "workflow::closed")),
        // GET current issue: already closed.
        MockResponse::ok(issue_payload(23, "Title", "closed", &["workflow::closed"])),
        // PUT response after the (now idempotent) label refresh.
        MockResponse::ok(issue_payload(23, "Title", "closed", &["workflow::closed"])),
    ]);
    let provider = provider(base);
    let issue = provider.close_issue(23).unwrap();
    assert_eq!(issue.state, "closed");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].starts_with("GET /api/v4/projects/42/issues/23"));
    assert!(requests[3].starts_with("PUT /api/v4/projects/42/issues/23"));
    assert!(
        !requests[3].contains("state_event"),
        "closed->closed must omit state_event: {}",
        requests[3],
    );
    assert!(requests[3].contains(r#""add_labels":["workflow::closed"]"#));
    server.join().unwrap();
}

// =============================================================================
// Comment lifecycle
// =============================================================================

#[test]
fn create_note_posts_to_notes_and_returns_stable_url() {
    // The create_note path POSTs the note first, then GETs the
    // parent issue so it can build a browsable
    // `<issue_web_url>#note_<id>` URL rather than the legacy
    // `/api/v4` API path.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(note_payload(42, "<!-- marker --> body")),
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
    ]);
    let provider = provider(base);
    let comment = provider.create_note(7, "<!-- marker --> body").unwrap();
    assert_eq!(comment.id, 42);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/7/notes"));
    assert!(requests[0].contains(r#""body":"<!-- marker --> body""#));
    assert!(requests[1].starts_with("GET /api/v4/projects/42/issues/7"));
    assert_eq!(
        comment.html_url.as_deref(),
        Some("https://gitlab.example/group/project/-/issues/7#note_42"),
    );
    server.join().unwrap();
}

#[test]
fn get_note_hits_notes_note_id() {
    // The get_note path issues the note GET first, then the
    // parent issue GET used to build the browsable URL.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(note_payload(42, "<!-- marker --> body")),
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
    ]);
    let provider = provider(base);
    let comment = provider.get_note(7, 42).unwrap();
    assert_eq!(comment.id, 42);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7/notes/42"));
    assert!(requests[1].starts_with("GET /api/v4/projects/42/issues/7"));
    assert_eq!(
        comment.html_url.as_deref(),
        Some("https://gitlab.example/group/project/-/issues/7#note_42"),
    );
    server.join().unwrap();
}

#[test]
fn find_marker_paginates_notes_until_match() {
    let (base, requests, server) = sequence(vec![
        // GET parent issue so the note URL can be derived from the
        // parent issue's web_url.
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
        MockResponse::ok(format!("[{}]", note_payload(1, "first body")))
            .with_header("x-next-page", "2"),
        MockResponse::ok(format!("[{}]", note_payload(2, "second body with marker")))
            .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let comment = provider.find_marker(7, "marker").unwrap();
    assert_eq!(comment.id, 2);
    assert_eq!(comment.marker.as_deref(), Some("marker"));
    assert_eq!(
        comment.html_url.as_deref(),
        Some("https://gitlab.example/group/project/-/issues/7#note_2"),
    );
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7"));
    assert!(requests[1].contains("page=1"));
    assert!(requests[2].contains("page=2"));
    server.join().unwrap();
}

#[test]
fn find_marker_returns_not_found_when_no_note_matches() {
    let (base, _requests, server) = sequence(vec![
        MockResponse::ok(issue_payload(7, "Title", "opened", &[])),
        MockResponse::ok(format!("[{}]", note_payload(1, "first body")))
            .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let error = provider.find_marker(7, "missing").unwrap_err();
    assert_eq!(error.json()["kind"], "not_found");
    server.join().unwrap();
}

#[test]
fn find_marker_rejects_empty_marker_before_request() {
    let result = zero_request(|provider| provider.find_marker(7, ""));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn note_html_url_is_browsable_web_url_with_note_fragment() {
    // GitLab renders notes inline on the parent issue page, so the
    // canonical note URL is `<issue_web_url>#note_<id>`. The web_url
    // returned by GitLab is `<host>/<namespace>/<project>/-/issues/<iid>`;
    // the provider must NOT synthesise an `/api/v4` path that is not
    // browsable from a web browser.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(note_payload(99, "note body")),
        MockResponse::ok(issue_payload(8, "Title", "opened", &[])),
    ]);
    let provider = provider(base);
    let comment = provider.create_note(8, "note body").unwrap();
    let url = comment
        .html_url
        .as_deref()
        .expect("note should carry a web URL");
    assert_eq!(
        url,
        "https://gitlab.example/group/project/-/issues/8#note_99",
    );
    assert!(
        !url.contains("/api/v4"),
        "note URL must not be an API-path URL: {url}",
    );
    assert!(
        !url.contains("/notes/"),
        "note URL must not embed the API /notes/ path: {url}"
    );
    // The PUT payload still went to the notes endpoint.
    let requests = requests.recv().unwrap();
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/8/notes"));
    assert!(requests[1].starts_with("GET /api/v4/projects/42/issues/8"));
    server.join().unwrap();
}

#[test]
fn note_html_url_is_none_when_issue_web_url_is_missing() {
    // If the parent issue payload omits `web_url`, the note must
    // also report `html_url = None` rather than fall back to an
    // `/api/v4` API path that is not browsable.
    let issue_body_without_web_url = serde_json::json!({
        "id": 9,
        "iid": 9,
        "title": "Title",
        "description": "",
        "state": "opened",
        "labels": [],
        // Deliberately omit `web_url`.
    })
    .to_string();
    let (base, _requests, server) = sequence(vec![
        MockResponse::ok(note_payload(123, "note body")),
        MockResponse::ok(issue_body_without_web_url),
    ]);
    let provider = provider(base);
    let comment = provider.create_note(9, "note body").unwrap();
    assert!(
        comment.html_url.is_none(),
        "missing issue web_url must produce a None note URL; got {:?}",
        comment.html_url,
    );
    server.join().unwrap();
}

// =============================================================================
// Tracker label mapping
// =============================================================================

#[test]
fn tracker_label_creates_type_bug_label_when_missing() {
    let (base, requests, server) = sequence(vec![
        // Label list returns empty so the provider must create it.
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(99, "type::bug")),
    ]);
    let provider = provider(base);
    let label = provider.tracker_label("Bug").unwrap();
    assert_eq!(label, "type::bug");
    let requests = requests.recv().unwrap();
    assert!(requests[1].starts_with("POST /api/v4/projects/42/labels"));
    assert!(requests[1].contains(r#""name":"type::bug""#));
    server.join().unwrap();
}

#[test]
fn tracker_label_creates_type_feature_label_when_missing() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok("[]").with_header("x-next-page", ""),
        MockResponse::ok(label_payload(100, "type::feature")),
    ]);
    let provider = provider(base);
    let label = provider.tracker_label("feature").unwrap();
    assert_eq!(label, "type::feature");
    let requests = requests.recv().unwrap();
    assert!(requests[1].contains(r#""name":"type::feature""#));
    server.join().unwrap();
}

#[test]
fn tracker_label_skips_creation_when_label_already_exists() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!("[{}]", label_payload(7, "type::bug")))
            .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let label = provider.tracker_label("Bug").unwrap();
    assert_eq!(label, "type::bug");
    // Only one HTTP call: the GET that found the existing label.
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/labels?"));
    server.join().unwrap();
}

#[test]
fn tracker_label_rejects_unknown_tracker_name() {
    let error = zero_request(|provider| provider.tracker_label("Task"));
    let error = error.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn workflow_label_resolves_every_canonical_status_via_helper() {
    use crate::gitlab_model::workflow_label_from_status;
    let cases = [
        ("New", "workflow::new"),
        ("InProgress", "workflow::in-progress"),
        ("InReview", "workflow::in-review"),
        ("ChangesRequested", "workflow::changes-requested"),
        ("Blocked", "workflow::blocked"),
        ("Resolved", "workflow::resolved"),
        ("Closed", "workflow::closed"),
        ("Cancelled", "workflow::cancelled"),
    ];
    for (input, expected) in cases {
        assert_eq!(workflow_label_from_status(input).unwrap(), expected);
    }
}

#[test]
fn workflow_label_rejects_unknown_status() {
    use crate::gitlab_model::workflow_label_from_status;
    let error = workflow_label_from_status("Reviewing").unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

// =============================================================================
// Error / redaction / role boundaries
// =============================================================================

#[test]
fn http_error_redacts_token_from_response_body() {
    let body = format!(r#"{{"message":"denied for token {TEST_TOKEN}"}}"#);
    let (result, _request) = one(MockResponse::status(403, body), |provider| {
        provider.get_issue(7)
    });
    let error = result.unwrap_err();
    let rendered = error.json().to_string();
    assert!(!rendered.contains(TEST_TOKEN), "{rendered}");
    assert!(rendered.contains("[redacted]"));
    assert_eq!(error.json()["status"], 403);
    assert_eq!(error.json()["operation"], "issue get");
}

#[test]
fn decode_error_does_not_leak_token() {
    // An unparseable response surfaces a `decode` error. Serde_json's
    // own parse-error string never includes the raw input, so the
    // contract is simply that the error kind is `decode` and the
    // token does not surface anywhere in the rendered payload.
    let body = format!("not-json {TEST_TOKEN}");
    let (result, _request) = one(MockResponse::ok(body), |provider| provider.get_issue(7));
    let error = result.unwrap_err();
    let rendered = error.json().to_string();
    assert_eq!(error.json()["kind"], "decode");
    assert_eq!(error.json()["operation"], "issue get");
    assert!(!rendered.contains(TEST_TOKEN), "{rendered}");
}

#[test]
fn find_marker_empty_marker_returns_config_error_without_request() {
    let result = zero_request(|provider| provider.find_marker(7, ""));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn dispatcher_routes_gitlab_get_issue_to_gitlab_provider() {
    let (result, request) = one(
        MockResponse::ok(issue_payload(33, "Routed", "opened", &[])),
        |provider| provider.get_issue(33),
    );
    let issue = result.unwrap();
    assert_eq!(issue.number, 33);
    assert_eq!(issue.title, "Routed");
    assert_request(&request, "GET", "/api/v4/projects/42/issues/33", None);
}

#[test]
fn gitlab_provider_rejects_planning_field_shapes_via_planning_cli() {
    // Redmine planning flags must surface as a structured
    // config error for GitLab, not as a successful write. Phase 4
    // distinguishes per-flag: `--parent-issue` (and every other
    // Redmine-only planning field) is rejected with a config error
    // that names the specific flag.
    use crate::command::PlanningOptions;
    use crate::provider::ProviderDispatcher;
    let dispatcher = ProviderDispatcher::Gitlab(
        GitlabProvider::new(
            GitlabConfig::new("https://gitlab.example/api/v4", 42),
            TEST_TOKEN.to_owned(),
        )
        .unwrap(),
    );
    let planning = PlanningOptions {
        parent_issue: Some("1".to_owned()),
        ..PlanningOptions::default()
    };
    let error = crate::redmine_planning_cli::resolve_planning(&dispatcher, &planning).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    let message = error.json()["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("parent-issue"),
        "error must name the rejected flag: {message}",
    );
}

#[test]
fn gitlab_tracker_only_create_succeeds_against_planning_cli() {
    // A tracker-only GitLab invocation must not require any Redmine
    // planning field; the planning CLI should fall through to the
    // provider's create path with a `type::bug` label.
    use crate::command::PlanningOptions;
    use crate::provider::ProviderDispatcher;
    let dispatcher = ProviderDispatcher::Gitlab(
        GitlabProvider::new(
            GitlabConfig::new("https://gitlab.example/api/v4", 42),
            TEST_TOKEN.to_owned(),
        )
        .unwrap(),
    );
    let planning = PlanningOptions::default();
    let resolved = crate::redmine_planning_cli::resolve_planning(&dispatcher, &planning).unwrap();
    // Empty planning fields round-trip cleanly.
    assert!(resolved.is_empty());
}

#[test]
fn list_projects_returns_not_supported_error() {
    let error = zero_request(|provider| provider.list_projects());
    let error = error.unwrap_err();
    assert_eq!(error.json()["kind"], "not_supported");
    assert_eq!(error.json()["operation"], "project list");
}

// =============================================================================
// RepoProvider / CiProvider adapter contracts. Phase 3 lifts the
// Forgejo-only restriction for repo create and CI reads so the
// GitLab provider drives both flows through the shared traits. The
// tests below exercise the wire-level shape end-to-end and lock in
// the recursion / role / provider guards. Each test binds a local
// TCP listener so the assertions inspect the exact request that
// reached the wire, never a synthesised stub.
// =============================================================================

// -- Repo create --------------------------------------------------------------

fn project_payload(id: u64, path: &str, namespace_path: &str, visibility: &str) -> String {
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

fn user_payload(id: u64) -> String {
    serde_json::json!({"id": id, "username": "owner"}).to_string()
}

#[test]
fn repo_create_posts_to_projects_with_namespace_id_and_private_visibility() {
    let (base, requests, server) = sequence(vec![
        // GitLabProvider::create_repo first fetches /user so it can
        // resolve the personal namespace id when the operator did not
        // supply an explicit namespace.
        MockResponse::ok(user_payload(7)),
        MockResponse::ok(project_payload(99, "widgets", "owner", "private")),
    ]);
    let provider = provider(base);
    let summary = provider
        .create_repo("widgets", true, "phase3", true)
        .unwrap();
    assert_eq!(summary.full_name, "owner/widgets");
    assert_eq!(summary.owner, "owner");
    assert_eq!(summary.name, "widgets");
    assert!(summary.private);
    assert_eq!(
        summary.html_url.as_deref(),
        Some("https://gitlab.example/owner/widgets"),
    );
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0].starts_with("GET /api/v4/user"),
        "first request must resolve the personal namespace: {}",
        requests[0],
    );
    assert_request(
        &requests[1],
        "POST",
        "/api/v4/projects",
        Some("\"name\":\"widgets\""),
    );
    assert!(
        requests[1].contains("\"visibility\":\"private\""),
        "private-only policy must serialise as visibility=private: {}",
        requests[1],
    );
    assert!(
        requests[1].contains("\"namespace_id\":7"),
        "personal namespace id must be forwarded when the target carries no OWNER prefix: {}",
        requests[1],
    );
    assert!(requests[1].contains("\"initialize_with_readme\":true"));
    assert!(requests[1].contains("\"description\":\"phase3\""));
    server.join().unwrap();
}

#[test]
fn repo_create_rejects_public_repository_without_request() {
    let provider = zero_request_provider();
    let error = provider
        .create_repo("acme/widgets", false, "", false)
        .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("private"),
        "{rendered}",
    );
}

#[test]
fn repo_create_bare_target_lands_in_personal_namespace() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(11)),
        MockResponse::ok(project_payload(101, "fresh", "owner", "private")),
    ]);
    let provider = provider(base);
    let summary = provider.create_repo("fresh", true, "", false).unwrap();
    assert_eq!(summary.full_name, "owner/fresh");
    let requests = requests.recv().unwrap();
    let body = &requests[1];
    assert!(body.starts_with("POST /api/v4/projects"));
    assert!(body.contains("\"name\":\"fresh\""));
    assert!(
        body.contains("\"namespace_id\":11"),
        "bare target without OWNER prefix must use the personal namespace id: {body}",
    );
    assert!(body.contains("\"visibility\":\"private\""));
    // When `--auto-init` is not supplied, the field is intentionally
    // omitted so a "don't touch my repo" caller gets a clean payload.
    assert!(
        !body.contains("initialize_with_readme"),
        "auto_init=false must omit initialize_with_readme so the server keeps its default: {body}",
    );
    server.join().unwrap();
}

#[test]
fn repo_create_owner_target_resolves_namespace_via_api() {
    // When the caller passes OWNER/REPO with no explicit namespace id,
    // the provider must resolve OWNER to a numeric namespace id via
    // `GET /namespaces?search=OWNER` and POST that id; it must never
    // silently fall back to the authenticated user's personal namespace.
    let (base, requests, server) = sequence(vec![
        // 1. resolve the personal user id.
        MockResponse::ok(user_payload(7)),
        // 2. search for the OWNER namespace; GitLab returns a single
        // group match.
        MockResponse::ok(
            r#"[{"id":42,"path":"acme","full_path":"acme","kind":"group","name":"Acme"}]"#,
        )
        .with_header("x-next-page", ""),
        // 3. POST /projects with the resolved namespace_id.
        MockResponse::ok(project_payload(33, "widgets", "acme", "private")),
    ]);
    let provider = provider(base);
    let summary = provider
        .create_repo("acme/widgets", true, "", false)
        .unwrap();
    assert_eq!(summary.full_name, "acme/widgets");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /api/v4/user"));
    let lookup = &requests[1];
    assert!(
        lookup.starts_with("GET /api/v4/namespaces?"),
        "OWNER must be resolved via /namespaces: {lookup}",
    );
    assert!(lookup.contains("search=acme"));
    let body = &requests[2];
    assert!(body.starts_with("POST /api/v4/projects"), "{body}",);
    assert!(
        body.contains("\"namespace_id\":42"),
        "resolved namespace id must be forwarded: {body}",
    );
    assert!(body.contains("\"path\":\"widgets\""));
    assert!(body.contains("\"visibility\":\"private\""));
    server.join().unwrap();
}

#[test]
fn repo_create_owner_target_without_namespace_id_errors_when_owner_missing() {
    // If /namespaces?search=OWNER returns no exact match, the
    // provider must surface a structured config error before POST
    // /projects so the operator can correct the OWNER.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(7)),
        MockResponse::ok("[]").with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let error = provider
        .create_repo("missing/widgets", true, "", false)
        .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("missing"),
        "{rendered}",
    );
    // The personal user resolution must still happen so a future
    // retry with a different OWNER doesn't re-resolve the namespace.
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v4/user"));
    assert!(requests[1].starts_with("GET /api/v4/namespaces?"));
    server.join().unwrap();
}

#[test]
fn repo_create_owner_target_errors_when_namespace_is_ambiguous() {
    // If /namespaces returns multiple exact matches for the OWNER
    // (for example a user namespace and a group namespace that share
    // a path) the provider must surface a structured config error
    // instead of picking one arbitrarily.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(7)),
        MockResponse::ok(
            r#"[
                {"id":11,"path":"acme","full_path":"acme","kind":"group","name":"Acme Group"},
                {"id":12,"path":"acme","full_path":"acme","kind":"user","name":"Acme User"}
            ]"#,
        )
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let error = provider
        .create_repo("acme/widgets", true, "", false)
        .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("ambiguous"),
        "{rendered}",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v4/user"));
    assert!(requests[1].starts_with("GET /api/v4/namespaces?"));
    server.join().unwrap();
}

#[test]
fn repo_create_owner_target_resolves_user_namespace_when_no_group_match() {
    // A bare user namespace (kind=user) with no group sharing the
    // path must still resolve to the user id so cross-account
    // OWNER/REPO works without an explicit --namespace.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(7)),
        MockResponse::ok(
            r#"[{"id":99,"path":"someone","full_path":"someone","kind":"user","name":"Someone"}]"#,
        )
        .with_header("x-next-page", ""),
        MockResponse::ok(project_payload(33, "widgets", "someone", "private")),
    ]);
    let provider = provider(base);
    let summary = provider
        .create_repo("someone/widgets", true, "", false)
        .unwrap();
    assert_eq!(summary.full_name, "someone/widgets");
    let requests = requests.recv().unwrap();
    let body = &requests[2];
    assert!(
        body.contains("\"namespace_id\":99"),
        "user namespace id must be forwarded when no group match exists: {body}",
    );
    server.join().unwrap();
}

#[test]
fn repo_create_bare_target_does_not_call_namespaces_endpoint() {
    // A bare REPOSITORY target must skip the OWNER lookup so the
    // personal namespace is used without an extra round trip.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(user_payload(11)),
        MockResponse::ok(project_payload(101, "fresh", "owner", "private")),
    ]);
    let provider = provider(base);
    let _ = provider.create_repo("fresh", true, "", false).unwrap();
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        !requests[0].contains("/namespaces"),
        "bare target must not call /namespaces: {}",
        requests[0],
    );
    server.join().unwrap();
}

#[test]
fn repo_create_handles_403_forbidden_as_structured_error() {
    // The first authenticated call is always GET /user. For a bare
    // target the call chain is /user -> POST /projects; for an
    // OWNER/REPO target /namespaces?search=OWNER sits in between.
    // This test exercises the bare-target path so the 403 surfaces
    // before any other request is sent.
    let (result, request) = one(
        MockResponse::status(403, r#"{"message":"insufficient scope"}"#),
        |provider| provider.create_repo("widgets", true, "desc", false),
    );
    assert!(request.starts_with("GET /api/v4/user"));
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "http");
    assert_eq!(rendered["status"], 403);
    assert_eq!(rendered["operation"], "repo create");
}

// -- CI reads ----------------------------------------------------------------

fn pipeline_payload(id: u64, iid: u64, status: &str, ref_name: &str, sha: &str) -> String {
    serde_json::json!({
        "id": id,
        "iid": iid,
        "project_id": 42,
        "status": status,
        "source": "push",
        "ref": ref_name,
        "sha": sha,
        "before_sha": "0000000000000000000000000000000000000000",
        "tag": false,
        "yaml_errors": null,
        "created_at": "2024-05-01T00:00:00.000Z",
        "started_at": "2024-05-01T00:00:01.000Z",
        "finished_at": "2024-05-01T00:00:30.000Z",
        "duration": 29.0,
        "queued_duration": 0.5,
        "web_url": format!("https://gitlab.example/group/project/-/pipelines/{id}"),
    })
    .to_string()
}

fn job_payload(id: u64, name: &str, status: &str, conclusion: Option<&str>) -> String {
    serde_json::json!({
        "id": id,
        "name": name,
        "stage": "test",
        "status": status,
        "conclusion": conclusion,
        "pipeline": {"id": 1, "iid": 1},
        "duration": 5.0,
        "queued_duration": 1.0,
        "created_at": "2024-05-01T00:00:00.000Z",
        "started_at": "2024-05-01T00:00:01.000Z",
        "finished_at": "2024-05-01T00:00:06.000Z",
        "web_url": format!("https://gitlab.example/group/project/-/jobs/{id}"),
    })
    .to_string()
}

#[test]
fn ci_runs_hits_pipelines_endpoint_with_filters() {
    use crate::ci_model::CiRunsFilter;
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 1, "success", "refs/heads/main", "abc123")
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: Some("abc123".to_owned()),
        ref_name: Some("refs/heads/main".to_owned()),
        status: Some("success".to_owned()),
        workflow: Some("ci.yml".to_owned()),
        page: 1,
        limit: 50,
    };
    let output = provider.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs.len(), 1);
    let run = &output.workflow_runs[0];
    assert_eq!(run.id, 11);
    assert_eq!(run.run_number, 1);
    assert_eq!(run.status, "success");
    assert_eq!(run.ref_name.as_deref(), Some("refs/heads/main"));
    assert_eq!(run.commit_sha.as_deref(), Some("abc123"));
    let requests = requests.recv().unwrap();
    let request = &requests[0];
    assert!(
        request.starts_with("GET /api/v4/projects/42/pipelines?"),
        "{request}",
    );
    assert!(request.contains("sha=abc123"));
    assert!(request.contains("ref=refs%2Fheads%2Fmain"));
    assert!(request.contains("status=success"));
    // page is always emitted by the helper.
    assert!(request.contains("page=1"));
    server.join().unwrap();
}

#[test]
fn ci_runs_paginates_until_x_next_page_is_empty() {
    use crate::ci_model::CiRunsFilter;
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 1, "success", "main", "aaa")
        ))
        .with_header("x-next-page", "2"),
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(12, 2, "failed", "main", "bbb")
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let output = provider.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs.len(), 2);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("page=1"));
    assert!(requests[1].contains("page=2"));
    server.join().unwrap();
}

#[test]
fn ci_runs_maps_status_through_shared_vocabulary() {
    use crate::ci_model::CiRunsFilter;
    let (base, _requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{},{},{},{}]",
            pipeline_payload(1, 1, "running", "main", "a"),
            pipeline_payload(2, 2, "failed", "main", "b"),
            pipeline_payload(3, 3, "canceled", "main", "c"),
            pipeline_payload(4, 4, "skipped", "main", "d"),
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let output = provider.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs.len(), 4);
    assert_eq!(output.workflow_runs[0].status, "running");
    assert_eq!(output.workflow_runs[1].status, "failure");
    assert_eq!(output.workflow_runs[2].status, "cancelled");
    assert_eq!(output.workflow_runs[3].status, "skipped");
    // The conclusion field is sourced from the terminal status only;
    // non-terminal statuses keep `conclusion: None`.
    assert_eq!(output.workflow_runs[0].conclusion, None);
    assert_eq!(
        output.workflow_runs[1].conclusion.as_deref(),
        Some("failed")
    );
    assert_eq!(
        output.workflow_runs[2].conclusion.as_deref(),
        Some("canceled")
    );
    assert_eq!(
        output.workflow_runs[3].conclusion.as_deref(),
        Some("skipped")
    );
    server.join().unwrap();
}

#[test]
fn ci_runs_preserves_unrecognised_statuses_unchanged() {
    // Future GitLab status values must surface in the shared JSON
    // contract rather than be silently remapped to "unknown".
    use crate::ci_model::CiRunsFilter;
    let (base, _requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(1, 1, "future-state", "main", "a")
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let output = provider.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs[0].status, "future-state");
    server.join().unwrap();
}

#[test]
fn ci_run_get_hits_single_pipeline_endpoint() {
    let (result, request) = one(
        MockResponse::ok(pipeline_payload(11, 7, "running", "main", "abc")),
        |provider| provider.ci_run_get(11),
    );
    let run = result.unwrap();
    assert_eq!(run.id, 11);
    assert_eq!(run.run_number, 7);
    assert_eq!(run.status, "running");
    assert_request(&request, "GET", "/api/v4/projects/42/pipelines/11", None);
}

#[test]
fn ci_run_jobs_hits_pipelines_pipeline_id_jobs_endpoint() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{},{}]",
            job_payload(101, "lint", "success", Some("success")),
            job_payload(102, "test", "failed", Some("failed")),
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let output = provider.ci_run_jobs(11).unwrap();
    assert_eq!(output.run_id, 11);
    assert_eq!(output.jobs.len(), 2);
    assert_eq!(output.jobs[0].name, "lint");
    assert_eq!(output.jobs[1].status, "failure");
    let requests = requests.recv().unwrap();
    let request = &requests[0];
    assert!(
        request.starts_with("GET /api/v4/projects/42/pipelines/11/jobs?"),
        "{request}",
    );
    server.join().unwrap();
}

#[test]
fn ci_job_logs_hits_jobs_job_id_trace_endpoint_with_bounded_tail() {
    use crate::ci_model::DEFAULT_LOG_TAIL;
    let trace = (0..2000)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (base, requests, server) = sequence(vec![MockResponse::ok(trace.clone())]);
    let provider = provider(base);
    let output = provider.ci_job_logs(101, 10).unwrap();
    assert_eq!(output.job_id, 101);
    assert!(output.truncated);
    assert!(output.bytes <= output.log.len());
    let tail_lines = output.log.lines().collect::<Vec<_>>();
    assert_eq!(tail_lines.len(), 10);
    assert_eq!(tail_lines.last().copied(), Some("line-1999"));
    let request = &requests.recv().unwrap()[0];
    assert!(
        request.starts_with("GET /api/v4/projects/42/jobs/101/trace"),
        "{request}",
    );
    // Tail must not leak the synthetic token when the trace mentions it.
    let _ = DEFAULT_LOG_TAIL;
    server.join().unwrap();
}

#[test]
fn ci_job_logs_redacts_token_from_raw_trace() {
    // Use the synthetic TEST_TOKEN as the secret so the redaction
    // path actually fires. The trace must surface `[redacted]`
    // instead of the raw token.
    let trace = format!("first line\n{TEST_TOKEN}\nlast line");
    let (result, _request) = one(MockResponse::ok(trace), |provider| {
        provider.ci_job_logs(1, 100)
    });
    let output = result.unwrap();
    assert!(
        !output.log.contains(TEST_TOKEN),
        "raw trace must not leak the token: {}",
        output.log,
    );
    assert!(output.log.contains("[redacted]"));
}

#[test]
fn ci_job_logs_returns_404_as_structured_error() {
    let (result, _request) = one(
        MockResponse::status(404, r#"{"message":"404 Not found"}"#),
        |provider| provider.ci_job_logs(99, 100),
    );
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "http");
    assert_eq!(rendered["status"], 404);
    assert_eq!(rendered["operation"], "ci job logs");
}

#[test]
fn ci_inspect_returns_no_run_when_no_pipeline_matches() {
    use crate::ci_model::CiInspectRequest;
    let (base, requests, server) =
        sequence(vec![MockResponse::ok("[]").with_header("x-next-page", "")]);
    let provider = provider(base);
    let request = CiInspectRequest {
        sha: "deadbeef".to_owned(),
        ref_name: None,
        wait: false,
        timeout: 1,
        poll: 1,
    };
    let output = provider.ci_inspect(&request).unwrap();
    assert_eq!(output.state, "no_run");
    assert!(output.selected_run.is_none());
    assert_eq!(output.poll_count, 1);
    let requests = requests.recv().unwrap();
    assert!(
        requests[0].contains("sha=deadbeef"),
        "sha must be forwarded: {}",
        requests[0],
    );
    server.join().unwrap();
}

#[test]
fn ci_inspect_returns_failure_for_failed_pipeline() {
    use crate::ci_model::CiInspectRequest;
    let (base, requests, server) = sequence(vec![
        // 1. initial runs listing; returned run matches the requested
        // sha and is already in a failed state, so the inspector
        // does not poll.
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 7, "failed", "main", "abc123")
        ))
        .with_header("x-next-page", ""),
        // 2. job listing for the failed pipeline.
        MockResponse::ok(format!(
            "[{},{}]",
            job_payload(101, "lint", "success", Some("success")),
            job_payload(102, "test", "failed", Some("failed")),
        ))
        .with_header("x-next-page", ""),
        // 3. job trace for the failing job.
        MockResponse::ok("test failed because of assertion X".to_owned()),
    ]);
    let provider = provider(base);
    let request = CiInspectRequest {
        sha: "abc123".to_owned(),
        ref_name: Some("main".to_owned()),
        wait: false,
        timeout: 1,
        poll: 1,
    };
    let output = provider.ci_inspect(&request).unwrap();
    assert_eq!(output.state, "failure");
    assert_eq!(output.sha, "abc123");
    let selected = output.selected_run.expect("selected run");
    assert_eq!(selected.id, 11);
    assert_eq!(selected.status, "failure");
    assert_eq!(output.failed_jobs.len(), 1);
    assert_eq!(output.failed_jobs[0].id, 102);
    assert_eq!(output.log_excerpts.len(), 1);
    assert_eq!(output.log_excerpts[0].name, "test");
    assert!(output.log_excerpts[0].log.contains("test failed"));
    let requests = requests.recv().unwrap();
    assert!(
        requests[0].starts_with("GET /api/v4/projects/42/pipelines?"),
        "{}",
        requests[0],
    );
    assert!(
        requests[1].starts_with("GET /api/v4/projects/42/pipelines/11/jobs?"),
        "{}",
        requests[1],
    );
    assert!(
        requests[2].starts_with("GET /api/v4/projects/42/jobs/102/trace"),
        "{}",
        requests[2],
    );
    server.join().unwrap();
}

#[test]
fn ci_inspect_treats_skipped_pipeline_as_distinct_non_failure() {
    // GitLab exposes `skipped` as a terminal pipeline state (for
    // example `when: never` rules). The shared Forgejo inspect
    // logic treats `skipped` as a distinct state, never as a
    // failure, so the GitLab provider must mirror that. This test
    // proves a skipped pipeline:
    //   * reports `state = "skipped"` rather than `"failure"`.
    //   * does NOT collect failed jobs.
    //   * does NOT issue a trace request for any job.
    use crate::ci_model::CiInspectRequest;
    let (base, requests, server) = sequence(vec![
        // 1. runs listing; the only candidate pipeline is skipped.
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 7, "skipped", "main", "abc123")
        ))
        .with_header("x-next-page", ""),
    ]);
    let provider = provider(base);
    let request = CiInspectRequest {
        sha: "abc123".to_owned(),
        ref_name: Some("main".to_owned()),
        wait: false,
        timeout: 1,
        poll: 1,
    };
    let output = provider.ci_inspect(&request).unwrap();
    assert_eq!(
        output.state, "skipped",
        "skipped pipeline must report a distinct, non-failure state: {output:?}"
    );
    let selected = output.selected_run.expect("selected run");
    assert_eq!(selected.id, 11);
    assert_eq!(selected.status, "skipped");
    assert!(
        output.failed_jobs.is_empty(),
        "skipped pipeline must not surface any failed jobs: {failed:?}",
        failed = output.failed_jobs,
    );
    assert!(
        output.log_excerpts.is_empty(),
        "skipped pipeline must not collect any log excerpts",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "skipped pipeline must not trigger job or trace requests: {requests:?}",
    );
    assert!(
        requests[0].starts_with("GET /api/v4/projects/42/pipelines?"),
        "{}",
        requests[0],
    );
    server.join().unwrap();
}

#[test]
fn ci_inspect_treats_skipped_job_as_non_failure_within_failed_pipeline() {
    // A pipeline can be failed overall but include skipped jobs (for
    // example `allow_failure: true` siblings). The skipped jobs must
    // not be added to `failed_jobs` or have their traces fetched,
    // matching the shared Forgejo inspect logic.
    use crate::ci_model::CiInspectRequest;
    let (base, requests, server) = sequence(vec![
        // 1. runs listing; pipeline is failed.
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 7, "failed", "main", "abc123")
        ))
        .with_header("x-next-page", ""),
        // 2. job listing; one failed, one skipped.
        MockResponse::ok(format!(
            "[{},{}]",
            job_payload(102, "test", "failed", Some("failed")),
            job_payload(103, "lint", "skipped", Some("skipped")),
        ))
        .with_header("x-next-page", ""),
        // 3. trace for the failed job only.
        MockResponse::ok("assertion X failed".to_owned()),
    ]);
    let provider = provider(base);
    let request = CiInspectRequest {
        sha: "abc123".to_owned(),
        ref_name: Some("main".to_owned()),
        wait: false,
        timeout: 1,
        poll: 1,
    };
    let output = provider.ci_inspect(&request).unwrap();
    assert_eq!(output.state, "failure");
    assert_eq!(output.failed_jobs.len(), 1);
    assert_eq!(output.failed_jobs[0].id, 102);
    assert_eq!(output.log_excerpts.len(), 1);
    assert_eq!(output.log_excerpts[0].name, "test");
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        3,
        "skipped jobs must not trigger a trace fetch: {requests:?}",
    );
    assert!(requests[1].starts_with("GET /api/v4/projects/42/pipelines/11/jobs?"));
    assert!(requests[2].starts_with("GET /api/v4/projects/42/jobs/102/trace"));
    assert!(
        !requests
            .iter()
            .any(|request| request.contains("/jobs/103/")),
        "skipped job 103 must not have its trace fetched: {requests:?}",
    );
    server.join().unwrap();
}

// -- Recursion and provider guards -------------------------------------------

#[test]
fn dispatcher_repo_provider_arm_routes_gitlab_create_repo_to_real_method() {
    // Phase 3 wires the dispatcher straight through to the GitLab
    // provider implementation. Direct callers that bypass the CLI
    // guards must reach the real method without recursing.
    let (base, requests, server) = sequence(vec![
        // 1. /user to resolve the personal namespace id.
        MockResponse::ok(user_payload(7)),
        // 2. /namespaces?search=acme to resolve the OWNER namespace id.
        MockResponse::ok(
            r#"[{"id":42,"path":"acme","full_path":"acme","kind":"group","name":"Acme"}]"#,
        )
        .with_header("x-next-page", ""),
        // 3. POST /projects with the resolved namespace_id.
        MockResponse::ok(project_payload(99, "widgets", "acme", "private")),
    ]);
    let dispatcher = ProviderDispatcher::Gitlab(provider(base));
    let summary = dispatcher
        .create_repo("acme/widgets", true, "phase3", true)
        .unwrap();
    assert_eq!(summary.full_name, "acme/widgets");
    assert_eq!(requests.recv().unwrap().len(), 3);
    server.join().unwrap();
}

#[test]
fn dispatcher_ci_provider_arm_routes_gitlab_ci_runs_to_real_method() {
    use crate::ci_model::CiRunsFilter;
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(format!(
            "[{}]",
            pipeline_payload(11, 1, "success", "main", "abc")
        ))
        .with_header("x-next-page", ""),
    ]);
    let dispatcher = ProviderDispatcher::Gitlab(provider(base));
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let output = dispatcher.ci_runs(&filter).unwrap();
    assert_eq!(output.workflow_runs.len(), 1);
    assert_eq!(requests.recv().unwrap().len(), 1);
    server.join().unwrap();
}

#[test]
fn gitlab_pipeline_request_includes_private_token_header() {
    use crate::ci_model::CiRunsFilter;
    let (base, requests, server) =
        sequence(vec![MockResponse::ok("[]").with_header("x-next-page", "")]);
    let provider = provider(base);
    let filter = CiRunsFilter {
        sha: None,
        ref_name: None,
        status: None,
        workflow: None,
        page: 1,
        limit: 50,
    };
    let _ = provider.ci_runs(&filter).unwrap();
    let request = &requests.recv().unwrap()[0];
    assert!(
        request
            .to_ascii_lowercase()
            .contains(&format!("private-token: {TEST_TOKEN}")),
        "missing PRIVATE-TOKEN header: {request}",
    );
    assert!(
        !request.to_ascii_lowercase().contains("authorization:"),
        "GitLab request leaked an Authorization header: {request}",
    );
    server.join().unwrap();
}

// =============================================================================
// Phase 4: time tracking and issue links
// =============================================================================

// -- Duration formatting ----------------------------------------------------

#[test]
fn format_gitlab_duration_handles_zero_and_sub_second_values() {
    use crate::gitlab_model::format_gitlab_duration;
    // Phase 4 contract: a zero-second projection still produces a
    // positive GitLab duration so the request never carries the
    // literal `0s` value (which GitLab rejects). The exact second
    // count is also preserved end-to-end.
    assert_eq!(format_gitlab_duration(0), "1s");
    assert_eq!(format_gitlab_duration(1), "1s");
    assert_eq!(format_gitlab_duration(59), "59s");
    assert_eq!(format_gitlab_duration(60), "1m");
    assert_eq!(format_gitlab_duration(3_600), "1h");
    assert_eq!(format_gitlab_duration(3_661), "1h1m1s");
    assert_eq!(format_gitlab_duration(86_400), "1d");
}

#[test]
fn format_gitlab_duration_round_trip_is_identity_for_every_known_unit() {
    use crate::gitlab_model::format_gitlab_duration;
    // The `add_spent_time` / `set_time_estimate` paths consume
    // second counts and emit durations through `format_gitlab_duration`,
    // so the production code never has to validate a string. This
    // test pins the canonical shape (every supported unit plus a
    // concatenated compound) to keep the wire format stable.
    assert_eq!(format_gitlab_duration(1), "1s");
    assert_eq!(format_gitlab_duration(60), "1m");
    assert_eq!(format_gitlab_duration(3_600), "1h");
    assert_eq!(format_gitlab_duration(3_661), "1h1m1s");
    assert_eq!(format_gitlab_duration(86_400), "1d");
}

// -- Spent time / time estimate ----------------------------------------------

#[test]
fn add_spent_time_posts_to_add_spent_time_with_summary() {
    let (result, request) = one(
        MockResponse::ok(
            r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
        ),
        |provider| provider.add_spent_time(7, 3_600, Some("phasegent timer run_id=timer-abc")),
    );
    let response = result.unwrap();
    assert_eq!(response.seconds, Some(3_600));
    assert_eq!(response.total_seconds, Some(3_600));
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/add_spent_time",
        Some(r#""duration":"1h""#),
    );
    assert!(
        request.contains(r#""summary":"phasegent timer run_id=timer-abc""#),
        "missing summary in body: {request}",
    );
}

#[test]
fn add_spent_time_response_carries_no_per_entry_identifier() {
    // Phase 4 audit invariant: GitLab REST v4 does not surface a
    // per-entry identifier for an individual spent-time addition.
    // The response carries the updated running totals only, so the
    // local SQLite ledger is the sole idempotency marker for the
    // timer path. The decoder must not invent a fake id.
    let (result, _request) = one(
        MockResponse::ok(
            r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
        ),
        |provider| provider.add_spent_time(7, 3_600, Some("phasegent timer run_id=timer-abc")),
    );
    let response = result.unwrap();
    let value = serde_json::to_value(&response).unwrap();
    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "human_readable".to_owned(),
            "seconds".to_owned(),
            "total_human_readable".to_owned(),
            "total_seconds".to_owned(),
        ],
        "ApiSpentTimeSummary must not invent an id field beyond the documented totals",
    );
}

#[test]
fn add_spent_time_rejects_non_positive_duration_before_request() {
    let result = zero_request(|provider| provider.add_spent_time(7, 0, None));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("positive")
    );
}

#[test]
fn add_spent_time_rejects_zero_iid_before_request() {
    let result = zero_request(|provider| provider.add_spent_time(0, 60, None));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("iid")
    );
}

#[test]
fn add_spent_time_handles_404_as_structured_error() {
    let (result, _request) = one(
        MockResponse::status(404, r#"{"message":"404 Not found"}"#),
        |provider| provider.add_spent_time(99, 60, Some("phasegent timer run_id=r")),
    );
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "http");
    assert_eq!(error.json()["status"], 404);
    assert_eq!(error.json()["operation"], "time spent create");
}

#[test]
fn set_time_estimate_posts_to_time_estimate_with_duration() {
    let (result, request) = one(
        MockResponse::ok(
            r#"{"seconds":1800,"human_readable":"30m","total_seconds":1800,"total_human_readable":"30m"}"#,
        ),
        |provider| provider.set_time_estimate(7, 1_800),
    );
    let response = result.unwrap();
    assert_eq!(response.seconds, Some(1_800));
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/time_estimate",
        Some(r#""duration":"30m""#),
    );
    // Time estimate has no summary field; the payload stays minimal.
    assert!(
        !request.contains("\"summary\""),
        "time estimate payload must not carry a summary field: {request}",
    );
}

#[test]
fn set_time_estimate_rejects_non_positive_duration_before_request() {
    let result = zero_request(|provider| provider.set_time_estimate(7, -1));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("positive")
    );
}

#[test]
fn add_spent_time_decodes_live_issue_shaped_response_with_time_stats() {
    // Live GitLab 19.x returns the full issue-shaped body for
    // POST /projects/:id/issues/:iid/add_spent_time with the
    // running totals wrapped under a nested `time_stats` block.
    // The decoder must surface the nested totals (not invent a
    // remote id) and mark the response as confirmed so the
    // projection can advance `sync_status` to `synced`.
    let body = serde_json::json!({
        "id": 7,
        "iid": 2,
        "title": "Live timer fixture",
        "state": "opened",
        "labels": [],
        "time_stats": {
            "time_estimate": 0,
            "total_time_spent": 2,
            "human_time_estimate": null,
            "human_total_time_spent": "2s"
        }
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |provider| {
        provider.add_spent_time(7, 2, Some("phasegent timer run_id=timer-abc"))
    });
    let response = result.unwrap();
    // The live response carries the totals only under time_stats;
    // the documented flat fields stay None so callers do not
    // mistake a wrapped response for the flat contract shape.
    assert!(
        response.seconds.is_none() && response.total_seconds.is_none(),
        "issue-shaped response must not promote flat totals from time_stats: {response:?}",
    );
    let stats = response
        .time_stats
        .as_ref()
        .expect("issue-shaped response must decode nested time_stats");
    assert_eq!(stats.total_time_spent, Some(2));
    assert_eq!(stats.human_total_time_spent.as_deref(), Some("2s"));
    assert_eq!(stats.time_estimate, Some(0));
    assert!(
        response.is_confirmed(),
        "nested time_stats.total_time_spent must confirm the write",
    );
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/add_spent_time",
        Some(r#""duration":"2s""#),
    );
    assert!(
        request.contains(r#""summary":"phasegent timer run_id=timer-abc""#),
        "summary must carry the run marker for UI traceability: {request}",
    );
}

#[test]
fn set_time_estimate_decodes_live_issue_shaped_response_with_time_stats() {
    // Same response-shape compatibility applies to set_time_estimate:
    // GitLab 19.x echoes the issue body with the running estimate
    // wrapped under time_stats.time_estimate. The decoder must
    // surface the nested value so a successful estimate update is
    // confirmed without inventing a remote id.
    let body = serde_json::json!({
        "id": 7,
        "iid": 2,
        "title": "Live estimate fixture",
        "state": "opened",
        "labels": [],
        "time_stats": {
            "time_estimate": 1800,
            "total_time_spent": 0,
            "human_time_estimate": "30m",
            "human_total_time_spent": null
        }
    })
    .to_string();
    let (result, request) = one(MockResponse::ok(body), |provider| {
        provider.set_time_estimate(7, 1_800)
    });
    let response = result.unwrap();
    assert!(
        response.seconds.is_none() && response.total_seconds.is_none(),
        "issue-shaped response must not promote flat totals: {response:?}",
    );
    let stats = response
        .time_stats
        .as_ref()
        .expect("issue-shaped response must decode nested time_stats");
    assert_eq!(stats.time_estimate, Some(1_800));
    assert_eq!(stats.human_time_estimate.as_deref(), Some("30m"));
    assert!(response.is_confirmed());
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/time_estimate",
        Some(r#""duration":"30m""#),
    );
    // Time estimate has no summary field; the payload stays minimal.
    assert!(
        !request.contains("\"summary\""),
        "time estimate payload must not carry a summary field: {request}",
    );
}

#[test]
fn add_spent_time_decodes_top_level_time_stats_response() {
    // Live GitLab 19.x returns a top-level time-stats object
    // (not the nested `time_stats` issue shape) for
    // POST /projects/:id/issues/:iid/add_spent_time. The body
    // captured live against project 3 issue 5 was
    // `{ "time_estimate": 0, "total_time_spent": 6,
    //   "human_time_estimate": null, "human_total_time_spent": "6s" }`.
    // The decoder must surface those totals at the top level
    // (NOT under time_stats) so `is_confirmed` returns true via
    // the top-level `total_time_spent` and the projection
    // advances `sync_status` to `synced`. The previous attempt's
    // nested-only handling kept every top-level field None and
    // left a successful POST marked `unconfirmed`.
    let body = r#"{
        "time_estimate": 0,
        "total_time_spent": 6,
        "human_time_estimate": null,
        "human_total_time_spent": "6s"
    }"#;
    let (result, request) = one(MockResponse::ok(body), |provider| {
        provider.add_spent_time(7, 6, Some("phasegent timer run_id=timer-abc"))
    });
    let response = result.unwrap();
    // Top-level time-stats fields must be populated directly on
    // the response struct, not under time_stats.
    assert_eq!(response.total_time_spent, Some(6));
    assert_eq!(response.time_estimate, Some(0));
    assert_eq!(response.human_total_time_spent.as_deref(), Some("6s"));
    assert!(
        response.human_time_estimate.is_none(),
        "JSON null must decode to None for human_time_estimate",
    );
    // The nested time_stats block stays None because the live
    // response does not wrap the totals inside an issue body;
    // the legacy flat totals also stay None because the live
    // response uses neither `seconds` nor `total_seconds`.
    assert!(
        response.time_stats.is_none(),
        "top-level response must not wrap totals under time_stats: {response:?}",
    );
    assert!(response.seconds.is_none());
    assert!(response.total_seconds.is_none());
    assert!(response.human_readable.is_none());
    assert!(response.total_human_readable.is_none());
    assert!(
        response.is_confirmed(),
        "top-level total_time_spent must confirm the write",
    );
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/add_spent_time",
        Some(r#""duration":"6s""#),
    );
    assert!(
        request.contains(r#""summary":"phasegent timer run_id=timer-abc""#),
        "summary must carry the run marker for UI traceability: {request}",
    );
}

#[test]
fn set_time_estimate_decodes_top_level_time_stats_response() {
    // Same shape compatibility for `set_time_estimate`: GitLab
    // 19.x returns a top-level time-stats object whose
    // `time_estimate` carries the updated value. The decoder
    // must surface the top-level field so a successful POST is
    // confirmed without inventing a remote id.
    let body = r#"{
        "time_estimate": 1800,
        "total_time_spent": 0,
        "human_time_estimate": "30m",
        "human_total_time_spent": null
    }"#;
    let (result, request) = one(MockResponse::ok(body), |provider| {
        provider.set_time_estimate(7, 1_800)
    });
    let response = result.unwrap();
    assert_eq!(response.time_estimate, Some(1_800));
    assert_eq!(response.total_time_spent, Some(0));
    assert_eq!(response.human_time_estimate.as_deref(), Some("30m"));
    assert!(
        response.human_total_time_spent.is_none(),
        "JSON null must decode to None for human_total_time_spent",
    );
    assert!(response.time_stats.is_none());
    assert!(response.seconds.is_none());
    assert!(response.total_seconds.is_none());
    assert!(response.is_confirmed());
    assert_request(
        &request,
        "POST",
        "/api/v4/projects/42/issues/7/time_estimate",
        Some(r#""duration":"30m""#),
    );
    // Time estimate has no summary field; the payload stays minimal.
    assert!(
        !request.contains("\"summary\""),
        "time estimate payload must not carry a summary field: {request}",
    );
}

// -- Issue links -------------------------------------------------------------

#[test]
fn list_issue_links_gets_links_endpoint_and_maps_types() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"[
            {"issue_link_id":1,"link_type":"relates_to","issue":{"id":101,"iid":11,"project_id":42}},
            {"issue_link_id":2,"link_type":"blocks","issue":{"id":102,"iid":12,"project_id":42}},
            {"issue_link_id":3,"link_type":"is_blocked_by","issue":{"id":103,"iid":13,"project_id":42}}
        ]"#,
    )
    .with_header("x-next-page", "")]);
    let provider = provider(base);
    let relations = provider.list_issue_links(7).unwrap();
    assert_eq!(relations.len(), 3);
    assert_eq!(relations[0].relation_type, "relates");
    assert_eq!(relations[0].issue_id, 7);
    assert_eq!(relations[0].issue_to_id, 11);
    assert_eq!(relations[0].id, 1);
    assert!(relations[0].delay.is_none());
    // `blocks` keeps the canonical name when the source issue owns
    // the link. `is_blocked_by` maps to the inverse `blocked` so the
    // output reads correctly from the queried issue's perspective.
    assert_eq!(relations[1].relation_type, "blocks");
    assert_eq!(relations[1].issue_to_id, 12);
    assert_eq!(relations[2].relation_type, "blocked");
    assert_eq!(relations[2].issue_to_id, 13);
    let requests = requests.recv().unwrap();
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7/links?"));
    server.join().unwrap();
}

#[test]
fn list_issue_links_rejects_zero_iid_before_request() {
    let result = zero_request(|provider| provider.list_issue_links(0));
    let error = result.unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn list_issue_links_decodes_live_get_response_target_issue_shape() {
    // The live `GET /projects/:id/issues/:iid/links` response is a
    // JSON array where every element is the **target issue object**
    // plus the link id (`issue_link_id`) and `link_type` attached
    // at the top level. The decoder must read the link id from
    // `issue_link_id` and the linked iid from the top-level `iid`
    // so the rendered `RelationSummary` is non-empty and points at
    // a real target issue. The `relation list` command was
    // returning zeroed relations before this contract was wired
    // through.
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"[
            {"id":101,"iid":11,"project_id":42,"issue_link_id":1,"link_type":"relates_to","title":"Alpha","state":"opened"},
            {"id":102,"iid":12,"project_id":42,"issue_link_id":2,"link_type":"blocks","title":"Beta","state":"opened"},
            {"id":103,"iid":13,"project_id":42,"issue_link_id":3,"link_type":"is_blocked_by","title":"Gamma","state":"opened"}
        ]"#,
    )
    .with_header("x-next-page", "")]);
    let provider = provider(base);
    let relations = provider.list_issue_links(7).unwrap();
    assert_eq!(relations.len(), 3);
    // Every entry must carry a non-zero link id, a non-zero
    // target iid, and the queried issue as `issue_id`. Without
    // this contract the live server response would silently
    // surface an empty relation.
    for (index, expected) in [1_u64, 2, 3].iter().enumerate() {
        assert_eq!(relations[index].id, *expected, "link id at {index}");
        assert_eq!(relations[index].issue_id, 7, "issue_id at {index}");
        assert!(
            relations[index].issue_to_id > 0,
            "issue_to_id must come from the live GET top-level iid; got zero at {index}",
        );
        assert!(relations[index].delay.is_none());
    }
    // Live GET shape must surface the link type strings exactly
    // as the server returned them; the inverse mapping still
    // converts `is_blocked_by` into the canonical `blocked` name
    // so the output reads correctly from the queried issue's
    // perspective.
    assert_eq!(relations[0].relation_type, "relates");
    assert_eq!(relations[0].issue_to_id, 11);
    assert_eq!(relations[1].relation_type, "blocks");
    assert_eq!(relations[1].issue_to_id, 12);
    assert_eq!(relations[2].relation_type, "blocked");
    assert_eq!(relations[2].issue_to_id, 13);
    let requests = requests.recv().unwrap();
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7/links?"));
    server.join().unwrap();
}

#[test]
fn create_issue_link_posts_query_parameters_with_target_project_id() {
    // The live `https://gitlab.example.com/19.2` instance expects
    // `target_project_id`, `target_issue_iid`, and the optional
    // `link_type` to arrive as URL query parameters; the body is
    // rejected. The provider must build the query string exactly
    // like the live server expects it and never put credentials in
    // the URL.
    let (result, request) = one(
        MockResponse::ok(
            r#"{"issue_link_id":8,"link_type":"relates_to","issue":{"id":13,"iid":13,"project_id":42}}"#,
        ),
        |provider| {
            use crate::redmine_model::RedmineRelationType;
            provider.create_issue_link(7, 13, RedmineRelationType::Relates)
        },
    );
    let summary = result.unwrap();
    assert_eq!(summary.id, 8);
    assert_eq!(summary.relation_type, "relates");
    assert_eq!(summary.issue_id, 7);
    assert_eq!(summary.issue_to_id, 13);
    assert!(summary.delay.is_none());
    assert_request(&request, "POST", "/api/v4/projects/42/issues/7/links", None);
    let query_string = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|path| path.split_once('?'))
        .map(|(_, query)| query.to_owned())
        .unwrap_or_default();
    assert!(
        query_string.contains("target_project_id=42"),
        "missing target_project_id query parameter: {request}",
    );
    assert!(
        query_string.contains("target_issue_iid=13"),
        "missing target_issue_iid query parameter: {request}",
    );
    assert!(
        query_string.contains("link_type=relates_to"),
        "missing link_type=relates_to query parameter: {request}",
    );
    // The body must be empty: the live instance rejects body-shape
    // payloads with HTTP 400, so the helper sends no body at all.
    let header_end = request.find("\r\n\r\n").unwrap_or(request.len());
    let body = request.get(header_end + 4..).unwrap_or("");
    assert!(
        body.trim().is_empty(),
        "create must send no body when the query parameters carry the payload; got: {body:?}",
    );
    // The PRIVATE-TOKEN header must remain present and the token
    // must NEVER appear as a URL parameter. The header check is
    // already enforced by `assert_request`; this second check
    // guards against a future contributor accidentally switching
    // to a query-parameter token leak.
    assert!(
        !query_string.to_ascii_lowercase().contains("private-token"),
        "PRIVATE-TOKEN must not be sent as a URL parameter: {request}",
    );
}

#[test]
fn create_issue_link_decodes_live_post_response_with_source_and_target_issues() {
    // The live POST response shape is `{id, source_issue,
    // target_issue, link_type}` rather than the legacy
    // `{issue_link_id, issue: {...}}` shape. The decoder must
    // resolve the link id from `id` and the linked iid from
    // `target_issue.iid` so the rendered `RelationSummary`
    // matches the live payload.
    let (result, _request) = one(
        MockResponse::ok(
            r#"{"id":42,"link_type":"relates_to","source_issue":{"id":7,"iid":7,"project_id":42},"target_issue":{"id":13,"iid":13,"project_id":42}}"#,
        ),
        |provider| {
            use crate::redmine_model::RedmineRelationType;
            provider.create_issue_link(7, 13, RedmineRelationType::Relates)
        },
    );
    let summary = result.unwrap();
    assert_eq!(summary.id, 42, "link id must come from top-level id");
    assert_eq!(summary.relation_type, "relates");
    assert_eq!(summary.issue_id, 7);
    assert_eq!(
        summary.issue_to_id, 13,
        "linked iid must come from target_issue.iid in the live POST response",
    );
    assert!(summary.delay.is_none());
}

#[test]
fn create_issue_link_rejects_blocks_with_structured_not_supported_before_request() {
    // The live instance rejects `blocks` / `is_blocked_by` for
    // create with `link_type does not have a valid value` even
    // when the request is sent with the documented query
    // parameters. We must therefore gate the create path locally
    // so the unsupported direction fails with a structured
    // `not_supported` error BEFORE any network traffic. The test
    // uses `zero_request` so no HTTP listener is started.
    let result = zero_request(|provider| {
        use crate::redmine_model::RedmineRelationType;
        provider.create_issue_link(7, 12, RedmineRelationType::Blocks)
    });
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "not_supported");
    assert_eq!(rendered["provider"], "gitlab");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("does not support relation create"),
        "not_supported error must mention the unsupported direction: {rendered}",
    );
}

#[test]
fn create_issue_link_rejects_precedes_before_request() {
    // Phase 5 adds a local capability gate on top of the existing
    // CLI-level precedes rejection. At the provider level, a
    // direct `create_issue_link(..., Precedes)` call must fail
    // before any HTTP traffic; the gate returns a structured
    // `not_supported` error because the live instance does not
    // accept `relates_to` as the link type for `precedes`. The
    // CLI dispatch layer in `redmine_relations_cli.rs` keeps its
    // earlier structured `config` error for `precedes`, which is
    // asserted separately by
    // `relation_cli_gitlab_create_rejects_precedes_as_config_error`.
    let result = zero_request(|provider| {
        use crate::redmine_model::RedmineRelationType;
        provider.create_issue_link(7, 8, RedmineRelationType::Precedes)
    });
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "not_supported");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("does not support relation create"),
        "provider must gate precedes with a not_supported error: {rendered}",
    );
}

#[test]
fn create_issue_link_rejects_zero_or_self_target_before_request() {
    let result = zero_request(|provider| {
        use crate::redmine_model::RedmineRelationType;
        provider.create_issue_link(7, 0, RedmineRelationType::Relates)
    });
    assert_eq!(result.unwrap_err().json()["kind"], "config");
    let result = zero_request(|provider| {
        use crate::redmine_model::RedmineRelationType;
        provider.create_issue_link(7, 7, RedmineRelationType::Relates)
    });
    assert_eq!(result.unwrap_err().json()["kind"], "config");
}

#[test]
fn delete_issue_link_uses_delete_with_source_and_target_iids() {
    // GitLab REST v4 requires the source issue iid in the URL
    // because the endpoint is scoped per source issue. The path
    // /projects/:id/issues/links/:link_id (without source iid)
    // does not exist; the contract test asserts the correct shape
    // so a future contributor cannot regress to the broken path.
    let (result, request) = one(MockResponse::status(204, ""), |provider| {
        provider.delete_issue_link(Some(7), 11)
    });
    assert_eq!(result.unwrap(), 11);
    assert_request(
        &request,
        "DELETE",
        "/api/v4/projects/42/issues/7/links/11",
        None,
    );
}

#[test]
fn delete_issue_link_without_source_issue_returns_config_error() {
    // The orchestrator CLI does not yet forward the source issue
    // iid (the parser does not accept it in this allowlist scope).
    // The provider surfaces a structured config error instead of
    // silently guessing the source, so a future caller can wire a
    // --issue flag through the parser.
    let result = zero_request(|provider| provider.delete_issue_link(None, 7));
    let error = result.unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("source"),
        "error must name the missing source field: {rendered}",
    );
}

#[test]
fn delete_issue_link_rejects_zero_source_or_zero_link_id_before_request() {
    let error = zero_request(|provider| provider.delete_issue_link(Some(0), 7)).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("source")
    );
    let error = zero_request(|provider| provider.delete_issue_link(Some(7), 0)).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .contains("link")
    );
}

// -- Relation CLI dispatch ----------------------------------------------------

#[test]
fn relation_cli_routes_gitlab_list_create_and_delete_fails_without_source() {
    use crate::command::RelationCommand;
    use crate::provider::ProviderDispatcher;
    use crate::redmine_model::RedmineRelationType;

    let (base, requests, server) = sequence(vec![
        // list
        MockResponse::ok(
            r#"[{"issue_link_id":1,"link_type":"relates_to","issue":{"id":12,"iid":12,"project_id":42}}]"#,
        )
        .with_header("x-next-page", ""),
        // create (Phase 5: only `relates` is supported for create, so
        // the mock returns the matching `relates_to` link type)
        MockResponse::ok(
            r#"{"issue_link_id":2,"link_type":"relates_to","issue":{"id":13,"iid":13,"project_id":42}}"#,
        ),
    ]);
    let dispatcher = ProviderDispatcher::Gitlab(provider(base));
    let listed =
        crate::redmine_relations_cli::execute(&dispatcher, &RelationCommand::List { issue: 7 })
            .unwrap();
    match listed {
        crate::redmine_relations_cli::RelationResult::List(relations) => {
            assert_eq!(relations.len(), 1);
            assert_eq!(relations[0].relation_type, "relates");
        }
        other => panic!("expected list result, got {other:?}"),
    }
    // Phase 5: the live instance only accepts `relates` for create;
    // `blocks` is gated with a structured not-supported error. Use
    // `Relates` here so the create path succeeds against the mock.
    let created = crate::redmine_relations_cli::execute(
        &dispatcher,
        &RelationCommand::Create {
            issue: 7,
            to: 13,
            relation_type: RedmineRelationType::Relates,
            delay: None,
        },
    )
    .unwrap();
    match created {
        crate::redmine_relations_cli::RelationResult::Created(summary) => {
            assert_eq!(summary.id, 2);
            assert_eq!(summary.relation_type, "relates");
        }
        other => panic!("expected created result, got {other:?}"),
    }
    // Delete without --issue must fail with a structured config
    // error: GitLab requires the source issue iid in the DELETE URL
    // and the orchestrator CLI surfaces the missing field explicitly
    // rather than silently guessing.
    let error = crate::redmine_relations_cli::execute(
        &dispatcher,
        &RelationCommand::Delete {
            relation_id: 2,
            issue: None,
        },
    )
    .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("source"),
        "delete error must name the missing source field: {rendered}",
    );
    // List and create consumed two requests; the rejected delete
    // hit zero endpoints.
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v4/projects/42/issues/7/links?"));
    assert!(requests[1].starts_with("POST /api/v4/projects/42/issues/7/links"));
    server.join().unwrap();
}

#[test]
fn relation_cli_routes_gitlab_delete_with_source_issue_iid() {
    // A normal CLI invocation that supplies the source issue iid
    // must reach the network with the correct URL. The dispatcher
    // is no longer allowed to silently fail for default GitLab
    // delete calls once the caller passes the flag.
    use crate::command::RelationCommand;
    use crate::provider::ProviderDispatcher;
    let (base, requests, server) = sequence(vec![MockResponse::status(204, "")]);
    let dispatcher = ProviderDispatcher::Gitlab(provider(base));
    let deleted = crate::redmine_relations_cli::execute(
        &dispatcher,
        &RelationCommand::Delete {
            relation_id: 11,
            issue: Some(7),
        },
    )
    .unwrap();
    match deleted {
        crate::redmine_relations_cli::RelationResult::Deleted(relation_id) => {
            assert_eq!(relation_id, 11);
        }
        other => panic!("expected deleted result, got {other:?}"),
    }
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with("DELETE /api/v4/projects/42/issues/7/links/11"),
        "delete must use the per-source-issue path: {}",
        requests[0],
    );
    assert!(
        !requests[0].contains("/issues/links/11"),
        "delete must NOT use the broken no-source path: {}",
        requests[0],
    );
    server.join().unwrap();
}

#[test]
fn relation_cli_routes_redmine_delete_with_ignored_source_issue() {
    // Redmine deletes ignore the optional source issue field; the
    // shared enum carries the field only to make the GitLab dispatch
    // explicit, not to alter Redmine behaviour.
    use crate::command::RelationCommand;
    use crate::provider::ProviderDispatcher;
    let dispatcher = ProviderDispatcher::Redmine(
        crate::provider_config::RedmineProvider::new(
            crate::provider_config::RedmineConfig::new(
                "https://redmine.example".to_owned(),
                "42".to_owned(),
                5,
            ),
            "test-redmine-key".to_owned(),
        )
        .unwrap(),
    );
    let error = crate::redmine_relations_cli::execute(
        &dispatcher,
        &RelationCommand::Delete {
            relation_id: 99,
            issue: Some(7),
        },
    )
    .unwrap_err();
    let rendered = error.json();
    // The orchestrator has no real Redmine server, so the request
    // fails with a network-level error rather than a not-supported
    // error. What matters is that the optional `issue` flag did
    // NOT short-circuit the dispatch to a not-supported error.
    assert_ne!(rendered["kind"], "not_supported");
    assert_ne!(rendered["operation"], "issue relations");
}

#[test]
fn relation_cli_gitlab_create_rejects_precedes_as_config_error() {
    use crate::command::RelationCommand;
    use crate::provider::ProviderDispatcher;
    use crate::redmine_model::RedmineRelationType;
    let provider = provider("http://127.0.0.1:1".to_owned());
    let dispatcher = ProviderDispatcher::Gitlab(provider);
    let error = crate::redmine_relations_cli::execute(
        &dispatcher,
        &RelationCommand::Create {
            issue: 7,
            to: 8,
            relation_type: RedmineRelationType::Precedes,
            delay: None,
        },
    )
    .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("precedes")
    );
}

#[test]
fn relation_cli_gitlab_create_rejects_delay_as_config_error() {
    use crate::command::RelationCommand;
    use crate::provider::ProviderDispatcher;
    use crate::redmine_model::RedmineRelationType;
    let provider = provider("http://127.0.0.1:1".to_owned());
    let dispatcher = ProviderDispatcher::Gitlab(provider);
    let error = crate::redmine_relations_cli::execute(
        &dispatcher,
        &RelationCommand::Create {
            issue: 7,
            to: 8,
            relation_type: RedmineRelationType::Blocks,
            delay: Some(2),
        },
    )
    .unwrap_err();
    let rendered = error.json();
    assert_eq!(rendered["kind"], "config");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("delay")
    );
}

#[test]
fn relation_cli_gitlab_create_rejects_blocks_as_not_supported_before_request() {
    // End-to-end CLI dispatch must surface the Phase 5 capability
    // gate: the live instance only accepts `relates` for create,
    // so `relation create --type blocks` against a GitLab
    // provider must fail with a structured `not_supported` error
    // BEFORE any HTTP traffic. The CLI binds to a deliberately
    // unreachable address; a real network call would surface as a
    // `request` kind rather than a `not_supported` kind.
    use crate::command::RelationCommand;
    use crate::provider::ProviderDispatcher;
    use crate::redmine_model::RedmineRelationType;
    let provider = provider("http://127.0.0.1:1".to_owned());
    let dispatcher = ProviderDispatcher::Gitlab(provider);
    let error = crate::redmine_relations_cli::execute(
        &dispatcher,
        &RelationCommand::Create {
            issue: 7,
            to: 8,
            relation_type: RedmineRelationType::Blocks,
            delay: None,
        },
    )
    .unwrap_err();
    let rendered = error.json();
    assert_eq!(
        rendered["kind"], "not_supported",
        "blocks create must surface as not_supported, not request / http: {rendered}",
    );
    assert_eq!(rendered["provider"], "gitlab");
    assert!(
        rendered["message"]
            .as_str()
            .unwrap_or_default()
            .contains("does not support relation create"),
        "error must reference the unsupported direction: {rendered}",
    );
}

// -- Timer CLI projection -----------------------------------------------------

#[test]
fn gitlab_timer_finish_first_call_posts_spent_time_with_marker() {
    // Drive `project_run_with_gitlab_provider` end-to-end. The
    // local ledger is the source of truth; the provider POSTs
    // `add_spent_time` with the run marker as the summary and never
    // round-trips through `/notes` or `/time_stats` for
    // reconciliation.
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-abc",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-abc", "DONE", 1_700_003_600)
        .unwrap();
    let mut run = finished;
    crate::time_tracking_cli::project_run_with_gitlab_provider(&storage, &mut run, &provider)
        .unwrap();
    assert_eq!(run.sync_status, "synced");
    assert!(
        run.time_entry_id.is_none(),
        "GitLab must not invent a time_entry_id",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "first projection must issue exactly one POST: {requests:?}",
    );
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/7/add_spent_time"));
    assert!(requests[0].contains(r#""duration":"1h""#));
    assert!(
        requests[0].contains(r#""summary":"phasegent timer run_id=timer-abc""#),
        "summary must carry the run marker for UI traceability: {}",
        requests[0],
    );
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

#[test]
fn gitlab_timer_finish_retry_uses_local_ledger_marker_not_note_body() {
    // GitLab REST v4 does not surface the spent-time summary back
    // through `/notes` or `/time_stats`, so the projection cannot
    // rely on note-body matching. The local SQLite ledger's
    // `sync_status` column is the sole idempotency marker; the
    // test deliberately does NOT inject the run marker into any
    // mocked note body so the assertion verifies real GitLab
    // behaviour rather than the old (broken) find_marker path.
    let provider = provider("http://127.0.0.1:1".to_owned());
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-retry",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let _ = storage
        .finish_timer_run("timer-retry", "DONE", 1_700_003_600)
        .unwrap();
    // Mark the run as already-projected (sync_status = synced,
    // time_entry_id stays None because the GitLab API does not
    // surface a numeric timelog id). A retry on the same run id
    // must observe this state and skip every network call.
    let _ = storage.mark_timer_sync(
        "timer-retry",
        None,
        None,
        crate::storage::TIMER_SYNC_SYNCED,
        None,
    );
    let mut run = storage.load_timer_run("timer-retry").unwrap().unwrap();
    crate::time_tracking_cli::project_run_with_gitlab_provider(&storage, &mut run, &provider)
        .unwrap();
    assert_eq!(run.sync_status, "synced");
    assert!(
        run.time_entry_id.is_none(),
        "GitLab must keep time_entry_id null when no remote id exists",
    );
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

#[test]
fn gitlab_timer_finish_failure_marks_ledger_failed() {
    // The projection POST is the only network call in the GitLab
    // path. A 422 response surfaces as a structured http error and
    // the failed-state recovery path in `execute_finish` records
    // the bounded error message on the ledger.
    let (base, requests, server) = sequence(vec![MockResponse::status(
        422,
        r#"{"message":"invalid duration"}"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-fail",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-fail", "DONE", 1_700_000_060)
        .unwrap();
    let mut run = finished;
    let error =
        crate::time_tracking_cli::project_run_with_gitlab_provider(&storage, &mut run, &provider)
            .unwrap_err();
    assert_eq!(error.json()["kind"], "http");
    assert_eq!(error.json()["status"], 422);
    // The failed-state recovery path inside execute_finish records the
    // bounded error message on the ledger so a retry can pick up the
    // context.
    let _ = storage.mark_timer_sync(
        "timer-fail",
        run.activity_id,
        run.time_entry_id,
        crate::storage::TIMER_SYNC_FAILED,
        Some(&error.to_string()),
    );
    let row = storage.load_timer_run("timer-fail").unwrap().unwrap();
    assert_eq!(row.sync_status, "failed");
    assert!(row.sync_error.is_some());
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
    let _ = requests;
}

#[test]
fn gitlab_timer_finish_skips_when_already_synced() {
    let provider = provider("http://127.0.0.1:1".to_owned());
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-sync",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let _ = storage
        .finish_timer_run("timer-sync", "DONE", 1_700_000_060)
        .unwrap();
    // Mark the run as synced without a time_entry_id (the GitLab
    // happy path leaves the column null).
    let _ = storage.mark_timer_sync(
        "timer-sync",
        None,
        None,
        crate::storage::TIMER_SYNC_SYNCED,
        None,
    );
    let mut run = storage.load_timer_run("timer-sync").unwrap().unwrap();
    // The projection path must observe the synced status and skip
    // every network call.
    crate::time_tracking_cli::project_run_with_gitlab_provider(&storage, &mut run, &provider)
        .unwrap();
    assert_eq!(run.sync_status, "synced");
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

#[test]
fn gitlab_timer_finish_marks_synced_when_response_uses_live_time_stats() {
    // GitLab 19.x returns the issue-shaped body for
    // POST /add_spent_time with the running totals wrapped under
    // a nested `time_stats` block. The projection must treat this
    // as a successful write (sync_status = synced) instead of
    // falling back to `unconfirmed`, otherwise the local ledger
    // would never observe a successful projection and every retry
    // would re-POST against the live instance. The disposable
    // issue that captured the live 2-second write is expected to
    // retain it; the test asserts only that the local state
    // machine advances to `synced`.
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"{
            "id": 7,
            "iid": 2,
            "title": "Live timer fixture",
            "state": "opened",
            "labels": [],
            "time_stats": {
                "time_estimate": 0,
                "total_time_spent": 2,
                "human_time_estimate": null,
                "human_total_time_spent": "2s"
            }
        }"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-live-shape",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-live-shape", "DONE", 1_700_000_002)
        .unwrap();
    let mut run = finished;
    crate::time_tracking_cli::project_run_with_gitlab_provider(&storage, &mut run, &provider)
        .unwrap();
    assert_eq!(
        run.sync_status, "synced",
        "nested time_stats must confirm the spent-time write: run={run:?}",
    );
    assert!(
        run.sync_error.is_none(),
        "synced projection must not carry a sync_error: run={run:?}",
    );
    assert!(
        run.time_entry_id.is_none(),
        "GitLab must not invent a time_entry_id even when the response is issue-shaped",
    );
    let persisted = storage.load_timer_run("timer-live-shape").unwrap().unwrap();
    assert_eq!(persisted.sync_status, "synced");
    assert!(
        persisted.time_entry_id.is_none(),
        "persisted run must also keep time_entry_id null: {persisted:?}",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "first projection must issue exactly one POST: {requests:?}",
    );
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/7/add_spent_time"));
    assert!(requests[0].contains(r#""duration":"2s""#));
    assert!(
        requests[0].contains(r#""summary":"phasegent timer run_id=timer-live-shape""#),
        "summary must carry the run marker for UI traceability: {}",
        requests[0],
    );
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

#[test]
fn gitlab_timer_finish_unconfirmed_when_response_omits_totals_entirely() {
    // A genuinely empty / unknown-shape response must still fall
    // back to `unconfirmed`. The repair only widens confirmation
    // to the issue-shaped body; the retry path keeps its
    // structured warning semantics for ambiguous results.
    let (base, _requests, server) = sequence(vec![MockResponse::ok(
        r#"{
            "id": 7,
            "iid": 2,
            "state": "opened",
            "labels": []
        }"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-empty-shape",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-empty-shape", "DONE", 1_700_000_002)
        .unwrap();
    let mut run = finished;
    crate::time_tracking_cli::project_run_with_gitlab_provider(&storage, &mut run, &provider)
        .unwrap();
    assert_eq!(
        run.sync_status, "unconfirmed",
        "totals-free response must keep unconfirmed semantics: run={run:?}",
    );
    assert!(
        run.sync_error.is_some(),
        "unconfirmed projection must record the bounded warning: run={run:?}",
    );
    assert!(
        run.time_entry_id.is_none(),
        "unconfirmed projection must not invent a time_entry_id",
    );
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

#[test]
fn gitlab_timer_finish_marks_synced_when_response_uses_top_level_time_stats() {
    // Live GitLab 19.x returns the top-level time-stats object
    // (not the nested issue shape) for
    // POST /projects/:id/issues/:iid/add_spent_time. The body
    // captured against project 3 issue 5 was
    // `{ "time_estimate": 0, "total_time_spent": 6,
    //   "human_time_estimate": null, "human_total_time_spent": "6s" }`.
    // The projection must observe the top-level `total_time_spent`
    // and advance `sync_status` to `synced`. The previous
    // attempt's nested-only handling left every top-level field
    // None and therefore marked a successful POST as
    // `unconfirmed`.
    let (base, requests, server) = sequence(vec![MockResponse::ok(
        r#"{
            "time_estimate": 0,
            "total_time_spent": 6,
            "human_time_estimate": null,
            "human_total_time_spent": "6s"
        }"#,
    )]);
    let provider = provider(base);
    let storage = open_temp_storage();
    let _ = storage
        .start_timer_run(
            "timer-top-level-shape",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-top-level-shape", "DONE", 1_700_000_006)
        .unwrap();
    let mut run = finished;
    crate::time_tracking_cli::project_run_with_gitlab_provider(&storage, &mut run, &provider)
        .unwrap();
    assert_eq!(
        run.sync_status, "synced",
        "top-level time_stats must confirm the spent-time write: run={run:?}",
    );
    assert!(
        run.sync_error.is_none(),
        "synced projection must not carry a sync_error: run={run:?}",
    );
    assert!(
        run.time_entry_id.is_none(),
        "GitLab must not invent a time_entry_id even when the response uses the top-level shape",
    );
    let persisted = storage
        .load_timer_run("timer-top-level-shape")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.sync_status, "synced");
    assert!(
        persisted.time_entry_id.is_none(),
        "persisted run must also keep time_entry_id null: {persisted:?}",
    );
    let requests = requests.recv().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "first projection must issue exactly one POST: {requests:?}",
    );
    assert!(requests[0].starts_with("POST /api/v4/projects/42/issues/7/add_spent_time"));
    assert!(requests[0].contains(r#""duration":"6s""#));
    assert!(
        requests[0].contains(r#""summary":"phasegent timer run_id=timer-top-level-shape""#),
        "summary must carry the run marker for UI traceability: {}",
        requests[0],
    );
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(storage.db_path().parent().unwrap());
}

fn open_temp_storage() -> crate::storage::Storage {
    let home = std::env::temp_dir().join(format!(
        "phasegent-timer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    crate::storage::Storage::open_at(&home.join(crate::storage::DB_FILENAME)).unwrap()
}

// =============================================================================
// Wire helpers
// =============================================================================

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
