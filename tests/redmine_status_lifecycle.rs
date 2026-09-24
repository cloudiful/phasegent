//! Black-box CLI integration tests for the Redmine issue status
//! lifecycle. These scenarios drive every canonical status name through
//! a real `phasegent status set` subprocess and assert that the close
//! paths verify the remote state, so a `200 OK` with a stale response
//! body never reports success. Shared scaffolding (constants, mock
//! server, SQLite fixture, binary runner) lives in `tests/support`.

#[path = "support/mod.rs"]
mod support;

use support::{
    CLOSE_STATUS_ID, ISSUE_ID, MockResponse, ORCHESTRATOR_KEY, PROJECT_ID, STATUS_BLOCKED,
    STATUS_CANCELLED, STATUS_CHANGES_REQUESTED, STATUS_CLOSED, STATUS_IN_PROGRESS,
    STATUS_IN_REVIEW, STATUS_NEW, STATUS_RESOLVED, issue_response_with_status, make_test_db,
    run_cli, start_mock_server, statuses_response, stderr_text, stdout_text,
};

/// Walk through the canonical Redmine status chain. Every transition
/// is driven by a real subprocess invocation of `phasegent status set`
/// so CLI parsing, provider dispatch, HTTP, status verification, and
/// JSON output all participate. The mock returns the issue with a
/// status id matching the request for each transition so the new
/// verification logic confirms the success.
#[test]
fn lifecycle_status_chain_drives_every_canonical_status() {
    let transitions: &[(&str, u64, bool)] = &[
        ("In Progress", STATUS_IN_PROGRESS, false),
        ("In Review", STATUS_IN_REVIEW, false),
        ("Changes Requested", STATUS_CHANGES_REQUESTED, false),
        ("Blocked", STATUS_BLOCKED, false),
        ("Resolved", STATUS_RESOLVED, true),
        ("Closed", STATUS_CLOSED, true),
        ("Cancelled", STATUS_CANCELLED, true),
        ("New", STATUS_NEW, false),
    ];

    // Three mock responses per transition (issue 394 P3 pre-write): GET
    // issue (scope guard), GET statuses, PUT issue. The mock returns the
    // issue with the *requested* status id so the verification logic accepts
    // the PUT response as authoritative; the guard only checks project.
    let mut responses = Vec::with_capacity(transitions.len() * 3);
    for (_name, status_id, is_closed) in transitions {
        responses.push(MockResponse::ok(issue_response_with_status(
            ISSUE_ID, *status_id, _name, *is_closed,
        )));
        responses.push(MockResponse::ok(statuses_response()));
        responses.push(MockResponse::ok(issue_response_with_status(
            ISSUE_ID, *status_id, _name, *is_closed,
        )));
    }

    let server = start_mock_server(responses);
    let db = make_test_db(&server.base_url);

    for (name, _status_id, is_closed) in transitions {
        let output = run_cli(
            &db.path,
            &server.base_url,
            Some("orchestrator"),
            &[
                "--provider",
                "redmine",
                "--project-id",
                PROJECT_ID,
                "status",
                "set",
                &ISSUE_ID.to_string(),
                "--status",
                name,
            ],
        );
        assert!(
            output.status.success(),
            "status set to {name} should succeed\nstdout: {}\nstderr: {}",
            stdout_text(&output),
            stderr_text(&output),
        );
        let json: serde_json::Value =
            serde_json::from_str(&stdout_text(&output)).expect("status set emits JSON");
        assert_eq!(json["number"], ISSUE_ID);
        let expected_state = if *is_closed { "closed" } else { "open" };
        assert_eq!(json["state"], expected_state, "expected state for {name}");
    }

    // The lifecycle ran through every status and each transition
    // produced exactly three requests (pre-write GET issue scope guard +
    // status list + status update PUT) because the close path runs through
    // `status set` rather than `issue close` here.
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        transitions.len() * 3,
        "expected {} requests, got {}",
        transitions.len() * 3,
        requests.len()
    );
    for (i, request) in requests.iter().enumerate() {
        if i % 3 == 0 {
            assert!(
                request.starts_with(&format!("GET /issues/{ISSUE_ID}.json")),
                "request {i} should be a pre-write scope-guard GET, got: {request}"
            );
            assert!(
                request.contains(&format!("x-redmine-api-key: {ORCHESTRATOR_KEY}")),
                "scope guard must use the orchestrator key (request {i}): {request}"
            );
        } else if i % 3 == 1 {
            assert!(
                request.starts_with("GET /issue_statuses.json"),
                "request {i} should be a status list GET, got: {request}"
            );
            assert!(
                request.contains(&format!("x-redmine-api-key: {ORCHESTRATOR_KEY}")),
                "status list must use the orchestrator key (request {i}): {request}"
            );
        } else {
            assert!(
                request.starts_with(&format!("PUT /issues/{ISSUE_ID}.json")),
                "request {i} should be a status update PUT, got: {request}"
            );
            assert!(
                request.contains(&format!("x-redmine-api-key: {ORCHESTRATOR_KEY}")),
                "status update must use the orchestrator key (request {i}): {request}"
            );
        }
    }

    // Drop the server first so the background thread shuts down before
    // the database is removed.
    drop(server);
}

/// `issue close` uses the configured close status id and verifies the
/// final remote state is actually closed. The mock returns the issue
/// with the close status id, so the close verification accepts the PUT
/// response as authoritative.
#[test]
fn issue_close_verifies_remote_state_through_subprocess() {
    // P3 pre-write: GET issue (scope guard) + PUT issue.
    let server = start_mock_server(vec![
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            CLOSE_STATUS_ID,
            "Closed",
            true,
        )),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            CLOSE_STATUS_ID,
            "Closed",
            true,
        )),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        Some("orchestrator"),
        &[
            "--provider",
            "redmine",
            "--project-id",
            PROJECT_ID,
            "issue",
            "close",
            &ISSUE_ID.to_string(),
        ],
    );
    assert!(
        output.status.success(),
        "close should succeed\nstdout: {}\nstderr: {}",
        stdout_text(&output),
        stderr_text(&output),
    );

    let json: serde_json::Value =
        serde_json::from_str(&stdout_text(&output)).expect("close emits JSON");
    assert_eq!(json["number"], ISSUE_ID);
    assert_eq!(json["state"], "closed");

    let requests = server.requests();
    assert_eq!(
        requests.len(),
        2,
        "close should produce pre-write GET + PUT: {requests:?}"
    );
    assert!(
        requests[0].starts_with(&format!("GET /issues/{ISSUE_ID}.json")),
        "first request must be the scope-guard GET: {}",
        requests[0]
    );
    let request = &requests[1];
    assert!(
        request.starts_with(&format!("PUT /issues/{ISSUE_ID}.json")),
        "close request: {request}"
    );
    assert!(
        request.contains(&format!("\"status_id\":{CLOSE_STATUS_ID}")),
        "close request must target the configured close status id: {request}"
    );
    assert!(
        request.contains(&format!("x-redmine-api-key: {ORCHESTRATOR_KEY}")),
        "close must use the orchestrator credential: {request}"
    );
}

/// The bug this issue tracks: a Redmine deployment that returns `200 OK`
/// from the PUT while leaving the issue in its previous status. The
/// mock echoes an issue with a stale status id; the binary must fail
/// with a non-zero exit code and a structured `request` error so the
/// operator never sees a false success.
#[test]
fn status_set_fails_when_remote_state_remains_stale() {
    // PUT response carries the issue in the *old* status (id=1, New)
    // even though the caller requested status_id=2 (In Progress).
    // P3 pre-write adds a leading GET issue (scope guard) with a passing
    // project so the flow still reaches the stale PUT.
    let stale_responses = vec![
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID, STATUS_NEW, "New", false,
        )),
        MockResponse::ok(statuses_response()),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID, STATUS_NEW, "New", false,
        )),
    ];
    let server = start_mock_server(stale_responses);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        Some("orchestrator"),
        &[
            "--provider",
            "redmine",
            "--project-id",
            PROJECT_ID,
            "status",
            "set",
            &ISSUE_ID.to_string(),
            "--status",
            "In Progress",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stale PUT response must surface a non-zero exit code\nstdout: {}\nstderr: {}",
        stdout_text(&output),
        stderr_text(&output),
    );
    let stderr = stderr_text(&output);
    let envelope: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("structured error envelope on stderr");
    assert_eq!(envelope["error"]["kind"], "request");
    assert_eq!(envelope["error"]["operation"], "issue status update");
    let message = envelope["error"]["message"]
        .as_str()
        .expect("error message present");
    assert!(
        message.contains(&STATUS_IN_PROGRESS.to_string()),
        "error message must mention the requested status id: {message}"
    );
    assert!(
        message.contains(&STATUS_NEW.to_string()),
        "error message must mention the observed (stale) status id: {message}"
    );
    // The mock should not have received a fourth request: a follow-up GET
    // would defeat the purpose of trusting the verified response.
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        3,
        "only the scope-guard GET + status list + PUT should fire; no follow-up GET expected when the PUT response already carries the mismatch id: {requests:?}"
    );
    assert!(
        requests[0].starts_with(&format!("GET /issues/{ISSUE_ID}.json")),
        "first request must be the scope-guard GET: {}",
        requests[0]
    );
}

/// `issue close` must also fail when the remote state does not match the
/// configured close status. The mock echoes an open status, so the new
/// close verification rejects the PUT response; the silent-200 mismatch
/// classifies as a workflow refusal and attempts the close climb, but the
/// exhausted mock returns no usable status list/issue so the climb falls
/// back to the legacy mismatch error instead of reporting success.
#[test]
fn issue_close_fails_when_remote_state_remains_open() {
    // P3 pre-write: leading GET passes the guard, PUT returns stale open.
    let server = start_mock_server(vec![
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID, STATUS_NEW, "New", false,
        )),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID, STATUS_NEW, "New", false,
        )),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        Some("orchestrator"),
        &[
            "--provider",
            "redmine",
            "--project-id",
            PROJECT_ID,
            "issue",
            "close",
            &ISSUE_ID.to_string(),
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "close with stale PUT response must surface a non-zero exit code\nstdout: {}\nstderr: {}",
        stdout_text(&output),
        stderr_text(&output),
    );
    let stderr = stderr_text(&output);
    let envelope: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("structured error envelope on stderr");
    assert_eq!(envelope["error"]["kind"], "request");
    assert_eq!(envelope["error"]["operation"], "issue close");
    let message = envelope["error"]["message"]
        .as_str()
        .expect("error message present");
    assert!(
        message.contains(&CLOSE_STATUS_ID.to_string()),
        "error message must mention the configured close status id: {message}"
    );

    let requests = server.requests();
    assert_eq!(
        requests.len(),
        4,
        "close should produce scope-guard GET + PUT + climb status list + climb current-issue GET: {requests:?}"
    );
    assert!(
        requests[0].starts_with(&format!("GET /issues/{ISSUE_ID}.json")),
        "first request must be the scope-guard GET: {}",
        requests[0]
    );
    assert!(
        requests[1].starts_with(&format!("PUT /issues/{ISSUE_ID}.json")),
        "second request must be the direct close PUT: {}",
        requests[1]
    );
    assert!(
        requests[2].starts_with("GET /issue_statuses.json"),
        "third request must be the climb status list: {}",
        requests[2]
    );
    assert!(
        requests[3].starts_with(&format!("GET /issues/{ISSUE_ID}.json")),
        "fourth request must be the climb current-issue read: {}",
        requests[3]
    );
}

/// `issue close` follows the same follow-up `GET` rule as `status set`:
/// when the PUT body is missing or empty the binary must re-read the
/// issue and confirm the close status. The mock returns no PUT body and
/// then an open follow-up GET; the mismatch climbs (exhausted mock, so
/// the climb falls back to the legacy error) and the close must fail.
#[test]
fn issue_close_fails_when_follow_up_get_shows_open_status() {
    // The binary will re-read on an empty PUT body; the follow-up GET
    // returns the issue still in an open state, so close fails.
    // P3 pre-write adds a leading GET (scope guard) before the PUT.
    let responses = vec![
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            STATUS_IN_PROGRESS,
            "In Progress",
            false,
        )),
        MockResponse::ok(""),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            STATUS_IN_PROGRESS,
            "In Progress",
            false,
        )),
    ];
    let server = start_mock_server(responses);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        Some("orchestrator"),
        &[
            "--provider",
            "redmine",
            "--project-id",
            PROJECT_ID,
            "issue",
            "close",
            &ISSUE_ID.to_string(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_text(&output);
    let envelope: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("structured error envelope on stderr");
    assert_eq!(envelope["error"]["kind"], "request");
    assert_eq!(envelope["error"]["operation"], "issue close");

    let requests = server.requests();
    assert_eq!(
        requests.len(),
        5,
        "scope-guard GET + PUT + follow-up GET + climb status list + climb current-issue GET"
    );
    assert!(requests[0].starts_with(&format!("GET /issues/{ISSUE_ID}.json")));
    assert!(requests[1].starts_with(&format!("PUT /issues/{ISSUE_ID}.json")));
    assert!(requests[2].starts_with(&format!("GET /issues/{ISSUE_ID}.json")));
    assert!(requests[3].starts_with("GET /issue_statuses.json"));
    assert!(requests[4].starts_with(&format!("GET /issues/{ISSUE_ID}.json")));
}
