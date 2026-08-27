//! Black-box CLI integration tests for the Redmine orchestrator-only
//! permission boundary and the status-list / binary-resolvability
//! baseline. These scenarios confirm that `status set` and `issue close`
//! reject every non-orchestrator role with a structured `permission`
//! error before any network call, and that the status-list path uses the
//! orchestrator credential end-to-end. Shared scaffolding lives in
//! `tests/support`.

#[path = "support/mod.rs"]
mod support;

use std::path::Path;

use support::{
    ADMIN_KEY, CLOSE_STATUS_ID, EXECUTOR_KEY, ISSUE_ID, MockResponse, ORCHESTRATOR_KEY, PROJECT_ID,
    REVIEWER_KEY, STATUS_BLOCKED, STATUS_CANCELLED, STATUS_CHANGES_REQUESTED, STATUS_CLOSED,
    STATUS_IN_PROGRESS, STATUS_IN_REVIEW, STATUS_NEW, STATUS_RESOLVED, issue_response_with_status,
    make_test_db, phasegent_bin, run_cli, start_mock_server, statuses_response, stderr_text,
    stdout_text,
};

/// `phasegent status set` is orchestrator-only. The executor, reviewer,
/// and admin roles must each be rejected with a structured permission
/// error before any network call. The mock records every request it
/// receives so the test asserts that no request fired for any of the
/// denied roles.
#[test]
fn status_set_denies_executor_reviewer_and_admin_with_structured_error() {
    // Mock with a single status list response so a hypothetical bug
    // that bypasses the permission check would still fail the
    // request-count assertion.
    let server = start_mock_server(vec![MockResponse::ok(statuses_response())]);
    let db = make_test_db(&server.base_url);

    let denied_roles = [
        ("executor", EXECUTOR_KEY),
        ("reviewer", REVIEWER_KEY),
        ("admin", ADMIN_KEY),
    ];
    for (role, expected_key) in denied_roles {
        let output = run_cli(
            &db.path,
            &server.base_url,
            &[
                "--role",
                role,
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
            Some(3),
            "{role} must be denied with exit code 3\nstdout: {}\nstderr: {}",
            stdout_text(&output),
            stderr_text(&output),
        );
        let stderr = stderr_text(&output);
        let envelope: serde_json::Value =
            serde_json::from_str(stderr.trim()).expect("structured error envelope on stderr");
        assert_eq!(
            envelope["error"]["kind"], "permission",
            "{role} denial must be a permission error: {stderr}"
        );
        assert_eq!(
            envelope["error"]["role"], role,
            "denial must echo the role: {stderr}"
        );
        assert_eq!(
            envelope["error"]["operation"], "issue status update",
            "denial must describe the operation: {stderr}"
        );
        // Guard against a future regression that leaks the credential
        // value into the error payload.
        assert!(
            !stderr.contains(expected_key),
            "{role} denial leaked the API key: {stderr}"
        );
        assert!(
            !stderr.contains(ORCHESTRATOR_KEY),
            "{role} denial leaked the orchestrator API key: {stderr}"
        );
    }

    assert_eq!(
        server.requests().len(),
        0,
        "denied roles must never reach the mock Redmine; observed requests: {:?}",
        server.requests(),
    );
}

/// `issue close` is also orchestrator-only. The executor, reviewer, and
/// admin roles must each be rejected with a structured permission error
/// before any network call.
#[test]
fn issue_close_denies_executor_reviewer_and_admin_with_structured_error() {
    let server = start_mock_server(vec![MockResponse::ok(issue_response_with_status(
        ISSUE_ID,
        CLOSE_STATUS_ID,
        "Closed",
        true,
    ))]);
    let db = make_test_db(&server.base_url);

    let denied_roles = [
        ("executor", EXECUTOR_KEY),
        ("reviewer", REVIEWER_KEY),
        ("admin", ADMIN_KEY),
    ];
    for (role, expected_key) in denied_roles {
        let output = run_cli(
            &db.path,
            &server.base_url,
            &[
                "--role",
                role,
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
            Some(3),
            "{role} must be denied with exit code 3\nstdout: {}\nstderr: {}",
            stdout_text(&output),
            stderr_text(&output),
        );
        let stderr = stderr_text(&output);
        let envelope: serde_json::Value =
            serde_json::from_str(stderr.trim()).expect("structured error envelope on stderr");
        assert_eq!(
            envelope["error"]["kind"], "permission",
            "{role} denial must be a permission error: {stderr}"
        );
        assert_eq!(envelope["error"]["role"], role);
        assert!(
            !stderr.contains(expected_key),
            "{role} denial leaked the API key: {stderr}"
        );
        assert!(
            !stderr.contains(ORCHESTRATOR_KEY),
            "{role} denial leaked the orchestrator API key: {stderr}"
        );
    }

    assert_eq!(
        server.requests().len(),
        0,
        "denied roles must never reach the mock Redmine; observed requests: {:?}",
        server.requests(),
    );
}

/// `status list` must use the orchestrator credential when reading the
/// canonical status list. This is a baseline assertion that the test
/// scaffolding itself is wired correctly: if the request key is wrong,
/// every other assertion in this file becomes meaningless.
#[test]
fn status_list_uses_orchestrator_credential_through_subprocess() {
    let server = start_mock_server(vec![MockResponse::ok(statuses_response())]);
    let db = make_test_db(&server.base_url);

    let output = run_cli(
        &db.path,
        &server.base_url,
        &[
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--project-id",
            PROJECT_ID,
            "status",
            "list",
        ],
    );
    assert!(output.status.success(), "status list should succeed");
    let json: serde_json::Value =
        serde_json::from_str(&stdout_text(&output)).expect("status list emits JSON");
    let statuses = json.as_array().expect("statuses JSON array");
    let ids: Vec<u64> = statuses
        .iter()
        .map(|status| status["id"].as_u64().expect("status id"))
        .collect();
    assert_eq!(
        ids,
        vec![
            STATUS_NEW,
            STATUS_IN_PROGRESS,
            STATUS_IN_REVIEW,
            STATUS_CHANGES_REQUESTED,
            STATUS_BLOCKED,
            STATUS_RESOLVED,
            STATUS_CLOSED,
            STATUS_CANCELLED,
        ]
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /issue_statuses.json"));
    assert!(
        requests[0].contains(&format!("x-redmine-api-key: {ORCHESTRATOR_KEY}")),
        "status list must use the orchestrator key: {}",
        requests[0],
    );
}

/// Sanity-check helper: assert that the current thread can read
/// `CARGO_BIN_EXE_phasegent` so the test binary does not silently fall
/// back to a wrong path.
#[test]
fn phasegent_binary_env_is_resolvable() {
    let path = phasegent_bin();
    assert!(
        Path::new(path).is_file(),
        "CARGO_BIN_EXE_phasegent ({path}) must point to a real binary",
    );
}
