//! Black-box CLI integration tests for the centralized status
//! transition policy: the idempotent same-status case, policy-legal and
//! policy-illegal `status advance` transitions, and preserved
//! server-side rejections. Shared scaffolding (mock server, SQLite
//! fixture, binary runner) lives in `tests/support`. The
//! `status next` read-only capability tests live in
//! `redmine_status_capability.rs` so this file stays under the 400-line
//! hard cap.

#[path = "support/mod.rs"]
mod support;

use support::{
    ISSUE_ID, MockResponse, PROJECT_ID, STATUS_IN_PROGRESS, STATUS_NEW, STATUS_RESOLVED,
    issue_response_with_status, make_test_db, run_cli, start_mock_server, statuses_response,
    stderr_text, stdout_text,
};

const POLICY_SOURCE: &str = "phasegent/canonical-phase-workflow@v1";

fn status_args<'a>(target: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec![
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "--project-id",
        PROJECT_ID,
        "status",
        "advance",
        "4242",
    ];
    if let Some(target) = target {
        args.push("--status");
        args.push(target);
    }
    args
}

fn put_requests(requests: &[String]) -> usize {
    requests
        .iter()
        .filter(|request| request.starts_with("PUT "))
        .count()
}

/// A policy-legal transition performs the PUT and reports the resolved
/// from/to statuses.
#[test]
fn status_advance_performs_policy_allowed_transition() {
    let server = start_mock_server(vec![
        MockResponse::ok(statuses_response()),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID, STATUS_NEW, "New", false,
        )),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            STATUS_IN_PROGRESS,
            "In Progress",
            false,
        )),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        &status_args(Some("In Progress")),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "advance failed: {}",
        stderr_text(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout_text(&output).trim()).expect("advance emits JSON");
    assert_eq!(json["changed"], true);
    assert_eq!(json["from"]["name"], "New");
    assert_eq!(json["to"]["name"], "In Progress");
    assert_eq!(json["to"]["id"], STATUS_IN_PROGRESS);
    assert_eq!(json["advisory"], false);
    assert_eq!(json["policy_source"], POLICY_SOURCE);
    assert_eq!(put_requests(&server.requests()), 1);
}

/// Advancing to the status the issue already has is an idempotent no-op:
/// no PUT is issued and the outcome reports `changed=false`.
#[test]
fn status_advance_same_status_is_idempotent_no_op() {
    let server = start_mock_server(vec![
        MockResponse::ok(statuses_response()),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            STATUS_IN_PROGRESS,
            "In Progress",
            false,
        )),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        &status_args(Some("In Progress")),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "same-status advance must succeed: {}",
        stderr_text(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout_text(&output).trim()).expect("advance emits JSON");
    assert_eq!(json["changed"], false);
    assert_eq!(json["from"]["name"], "In Progress");
    assert_eq!(json["to"]["name"], "In Progress");
    assert_eq!(
        put_requests(&server.requests()),
        0,
        "an idempotent no-op must not write"
    );
}

/// The phase-continuation edge is a legal policy transition: a reviewed
/// phase in `Resolved` must be able to return to `In Progress` for the
/// next remaining phase, so the PUT is attempted instead of being
/// rejected by policy.
#[test]
fn status_advance_performs_resolved_to_in_progress_phase_continuation() {
    let server = start_mock_server(vec![
        MockResponse::ok(statuses_response()),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            STATUS_RESOLVED,
            "Resolved",
            true,
        )),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            STATUS_IN_PROGRESS,
            "In Progress",
            false,
        )),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        &status_args(Some("In Progress")),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "phase continuation must be policy-legal: {}",
        stderr_text(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout_text(&output).trim()).expect("advance emits JSON");
    assert_eq!(json["changed"], true);
    assert_eq!(json["from"]["name"], "Resolved");
    assert_eq!(json["to"]["name"], "In Progress");
    assert_eq!(json["to"]["id"], STATUS_IN_PROGRESS);
    assert_eq!(
        json["advisory"], false,
        "both sides are canonical, so the verdict is not advisory"
    );
    assert_eq!(put_requests(&server.requests()), 1);
}

/// Policy legality is not a claim about the installation: if the Redmine
/// workflow refuses the continuation edge, the server failure survives
/// with bounded transition context instead of being masked.
#[test]
fn status_advance_preserves_server_rejection_of_phase_continuation() {
    let server = start_mock_server(vec![
        MockResponse::ok(statuses_response()),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            STATUS_RESOLVED,
            "Resolved",
            true,
        )),
        MockResponse {
            status: 422,
            body: serde_json::json!({"errors": ["Status is invalid"]}).to_string(),
        },
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        &status_args(Some("In Progress")),
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_text(&output);
    let json: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("server rejection emits structured JSON");
    assert_eq!(json["error"]["status"], 422);
    let message = json["error"]["message"]
        .as_str()
        .expect("structured error carries a message")
        .to_owned();
    assert!(
        message.contains("Status is invalid") && message.contains("authoritative"),
        "the Redmine workflow must stay authoritative: {message}"
    );
    assert!(
        message.contains("'Resolved'") && message.contains("status next 4242"),
        "server rejection must gain bounded transition context: {message}"
    );
}

/// A policy-illegal transition fails before the PUT with structured
/// guidance naming current, target, allowed_next, the policy source, and
/// the recovery command.
#[test]
fn status_advance_rejects_illegal_transition_before_any_write() {
    let server = start_mock_server(vec![
        MockResponse::ok(statuses_response()),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            STATUS_RESOLVED,
            "Resolved",
            true,
        )),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(&db.path, &server.base_url, &status_args(Some("In Review")));
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_text(&output);
    let json: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("illegal transition emits structured JSON");
    let message = json["error"]["message"]
        .as_str()
        .expect("structured error carries a message")
        .to_owned();
    for expected in [
        "'Resolved'",
        "'In Review'",
        "allowed_next=[In Progress, Closed]",
        POLICY_SOURCE,
        "status next 4242",
    ] {
        assert!(
            message.contains(expected),
            "message must contain {expected}: {message}"
        );
    }
    assert_eq!(
        put_requests(&server.requests()),
        0,
        "an illegal transition must fail before any write"
    );
}

/// A terminal status rejects every transition and says so explicitly.
#[test]
fn status_advance_rejects_transition_out_of_terminal_status() {
    let server = start_mock_server(vec![
        MockResponse::ok(statuses_response()),
        MockResponse::ok(issue_response_with_status(ISSUE_ID, 8, "Cancelled", true)),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        &status_args(Some("In Progress")),
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_text(&output);
    assert!(
        stderr.contains("terminal status"),
        "terminal rejection must be explicit: {stderr}"
    );
    assert_eq!(put_requests(&server.requests()), 0);
}

/// A custom status is forwarded to the server as advisory: policy cannot
/// judge it, so the transition is attempted and the response flags the
/// caveat.
#[test]
fn status_advance_forwards_custom_status_as_advisory() {
    let statuses = serde_json::json!({
        "issue_statuses": [
            {"id": 91, "name": "Triaged", "is_closed": false},
            {"id": 92, "name": "In Progress", "is_closed": false},
        ]
    })
    .to_string();
    let server = start_mock_server(vec![
        MockResponse::ok(statuses),
        MockResponse::ok(issue_response_with_status(ISSUE_ID, 91, "Triaged", false)),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID,
            92,
            "In Progress",
            false,
        )),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        &status_args(Some("In Progress")),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "advisory transition must reach the server: {}",
        stderr_text(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout_text(&output).trim()).expect("advance emits JSON");
    assert_eq!(json["changed"], true);
    assert_eq!(json["advisory"], true);
    assert!(json["caveat"].is_string());
    assert_eq!(put_requests(&server.requests()), 1);
}

/// A server-side rejection of a policy-allowed transition keeps the
/// original Redmine failure and appends bounded current/target/recovery
/// context instead of replacing it with generic policy text.
#[test]
fn status_advance_preserves_server_rejection_with_added_context() {
    let server = start_mock_server(vec![
        MockResponse::ok(statuses_response()),
        MockResponse::ok(issue_response_with_status(
            ISSUE_ID, STATUS_NEW, "New", false,
        )),
        MockResponse {
            status: 422,
            body: serde_json::json!({"errors": ["Status is invalid"]}).to_string(),
        },
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        &status_args(Some("In Progress")),
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_text(&output);
    let json: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("server rejection emits structured JSON");
    assert_eq!(json["error"]["status"], 422);
    let message = json["error"]["message"]
        .as_str()
        .expect("structured error carries a message")
        .to_owned();
    assert!(
        message.contains("Status is invalid"),
        "the original server failure must survive: {message}"
    );
    assert!(
        message.contains("'In Progress'") && message.contains("status next 4242"),
        "server rejection must gain bounded transition context: {message}"
    );
}
