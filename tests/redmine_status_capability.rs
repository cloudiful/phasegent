//! Black-box CLI integration tests for the `status next` read-only
//! capability: current status, policy-allowed next statuses resolved to
//! this installation's ids, policy source, server caveat, and recovery
//! command. Shared scaffolding (mock server, SQLite fixture, binary
//! runner) lives in `tests/support`.

#[path = "support/mod.rs"]
mod support;

use support::{
    ISSUE_ID, MockResponse, PROJECT_ID, STATUS_BLOCKED, STATUS_CANCELLED, STATUS_CLOSED,
    STATUS_IN_PROGRESS, STATUS_IN_REVIEW, STATUS_RESOLVED, issue_response_with_status,
    make_test_db, run_cli, start_mock_server, statuses_response, stderr_text, stdout_text,
};

const POLICY_SOURCE: &str = "phasegent/canonical-phase-workflow@v1";

fn status_args<'a>(command: &'a str) -> Vec<&'a str> {
    vec![
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "--project-id",
        PROJECT_ID,
        "status",
        command,
        "4242",
    ]
}

fn put_requests(requests: &[String]) -> usize {
    requests
        .iter()
        .filter(|request| request.starts_with("PUT "))
        .count()
}

/// `status next` reports the current status, the policy-allowed next
/// statuses resolved to this installation's ids, the policy source, the
/// server caveat, and a concrete recovery command.
#[test]
fn status_next_reports_current_and_allowed_next_with_installation_ids() {
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

    let output = run_cli(&db.path, &server.base_url, &status_args("next"));
    assert_eq!(
        output.status.code(),
        Some(0),
        "status next failed: {}",
        stderr_text(&output)
    );

    let json: serde_json::Value =
        serde_json::from_str(stdout_text(&output).trim()).expect("status next emits JSON");
    assert_eq!(json["issue"], ISSUE_ID);
    assert_eq!(json["current"]["name"], "In Progress");
    assert_eq!(json["current"]["id"], STATUS_IN_PROGRESS);
    assert_eq!(json["current"]["canonical"], true);
    assert_eq!(json["advisory"], false);
    assert_eq!(json["policy_source"], POLICY_SOURCE);

    let allowed = json["allowed_next"].as_array().expect("allowed_next array");
    let pairs = allowed
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().unwrap_or_default().to_owned(),
                entry["id"].as_u64().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        vec![
            ("In Review".to_owned(), STATUS_IN_REVIEW),
            ("Blocked".to_owned(), STATUS_BLOCKED),
            ("Cancelled".to_owned(), STATUS_CANCELLED),
        ]
    );
    assert!(
        json["caveat"]
            .as_str()
            .unwrap_or_default()
            .contains("authoritative"),
        "caveat must flag that the server workflow is authoritative"
    );
    assert!(
        json["recovery"]
            .as_str()
            .unwrap_or_default()
            .contains("status next 4242"),
        "recovery must name the concrete command"
    );
    // Read-only capability: no write may reach the server.
    assert_eq!(put_requests(&server.requests()), 0);
}

/// `Resolved` is a per-phase checkpoint, so `status next` must report
/// both installation-resolved targets: the phase-continuation edge back
/// to `In Progress` and the task-final `Closed` edge.
#[test]
fn status_next_reports_resolved_continuation_and_final_close() {
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

    let output = run_cli(&db.path, &server.base_url, &status_args("next"));
    assert_eq!(
        output.status.code(),
        Some(0),
        "status next failed: {}",
        stderr_text(&output)
    );

    let json: serde_json::Value =
        serde_json::from_str(stdout_text(&output).trim()).expect("status next emits JSON");
    assert_eq!(json["current"]["name"], "Resolved");
    assert_eq!(json["current"]["canonical"], true);
    assert_eq!(json["advisory"], false);
    assert_eq!(json["policy_source"], POLICY_SOURCE);

    let allowed = json["allowed_next"].as_array().expect("allowed_next array");
    let pairs = allowed
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().unwrap_or_default().to_owned(),
                entry["id"].as_u64().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        vec![
            ("In Progress".to_owned(), STATUS_IN_PROGRESS),
            ("Closed".to_owned(), STATUS_CLOSED),
        ],
        "a reviewed phase must be able to continue as well as close"
    );
    assert!(
        json["allowed_next_missing_on_server"].is_null(),
        "this installation defines both targets"
    );
    assert!(
        json["caveat"]
            .as_str()
            .unwrap_or_default()
            .contains("authoritative"),
        "the server workflow stays authoritative for the continuation edge"
    );
    assert_eq!(put_requests(&server.requests()), 0);
}

/// A terminal status reports an empty `allowed_next` instead of
/// pretending a transition is available.
#[test]
fn status_next_reports_terminal_status_with_no_allowed_next() {
    let server = start_mock_server(vec![
        MockResponse::ok(statuses_response()),
        MockResponse::ok(issue_response_with_status(ISSUE_ID, 7, "Closed", true)),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(&db.path, &server.base_url, &status_args("next"));
    let json: serde_json::Value =
        serde_json::from_str(stdout_text(&output).trim()).expect("status next emits JSON");
    assert_eq!(json["current"]["name"], "Closed");
    assert_eq!(json["current"]["canonical"], true);
    assert_eq!(json["advisory"], false);
    assert!(
        json["allowed_next"]
            .as_array()
            .expect("allowed_next array")
            .is_empty()
    );
}

/// A custom, non-canonical status is identified as advisory and
/// server-controlled rather than silently claimed allowed.
#[test]
fn status_next_marks_custom_status_as_advisory() {
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
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(&db.path, &server.base_url, &status_args("next"));
    let json: serde_json::Value =
        serde_json::from_str(stdout_text(&output).trim()).expect("status next emits JSON");
    assert_eq!(json["current"]["name"], "Triaged");
    assert_eq!(json["current"]["canonical"], false);
    assert_eq!(json["advisory"], true);
    assert!(
        json["allowed_next"]
            .as_array()
            .expect("allowed_next array")
            .is_empty()
    );
}

/// `status next` resolves the policy names against installation-specific
/// numeric ids, not the ids used by any other Redmine instance.
#[test]
fn status_next_resolves_installation_specific_ids_and_reports_missing_names() {
    let statuses = serde_json::json!({
        "issue_statuses": [
            {"id": 501, "name": "New", "is_closed": false},
            {"id": 502, "name": "In Progress", "is_closed": false},
        ]
    })
    .to_string();
    let server = start_mock_server(vec![
        MockResponse::ok(statuses),
        MockResponse::ok(issue_response_with_status(ISSUE_ID, 501, "New", false)),
    ]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(&db.path, &server.base_url, &status_args("next"));
    let json: serde_json::Value =
        serde_json::from_str(stdout_text(&output).trim()).expect("status next emits JSON");
    assert_eq!(json["current"]["id"], 501);
    let allowed = json["allowed_next"].as_array().expect("allowed_next array");
    assert_eq!(allowed.len(), 1);
    assert_eq!(allowed[0]["name"], "In Progress");
    assert_eq!(allowed[0]["id"], 502);
    assert_eq!(
        json["allowed_next_missing_on_server"],
        serde_json::json!(["Cancelled"]),
        "a policy status this installation lacks must be reported by name"
    );
}
