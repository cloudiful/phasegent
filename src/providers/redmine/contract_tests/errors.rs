#![allow(unused_imports)]
use super::support;
use super::support::{
    MockResponse, TEST_API_KEY, current_user_response, git_mirror_response, issue_collection,
    issue_response, membership_collection, membership_collection_page, mirror_env, one,
    project_collection, project_response, provider, role_collection, role_collection_page,
    sequence, strings, time_entry_activities, time_entry_collection, time_entry_response,
    user_from_response, version_collection, version_collection_page,
};
use crate::auth;
use crate::command::{
    self, Command, IssueCommand, ProjectCommand, RelationCommand, StatusCommand, WorkflowCommand,
};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{Storage, TimerRun};
use crate::policy::{Capability, Role};
use crate::providers::redmine::model::{RedmineRelationType, RedmineTimeEntryActivity};
use crate::providers::{
    ProviderDispatcher, ProviderKind, RedmineConfig, RedmineIssueStatus, RedmineMetadataProvider,
    RedmineProvider,
};
use std::str::FromStr;
use std::{fs, time};

#[test]
fn redmine_errors_decode_arrays_and_redact_api_key() {
    let response = MockResponse::error(
        422,
        format!(r#"{{"errors":["bad {TEST_API_KEY}",{{"message":"invalid project"}}]}}"#),
    );
    let (result, request) = one(response, |redmine| redmine.get_issue(22));
    let error = result.unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 422);
    assert!(json["message"].as_str().unwrap().contains("bad [redacted]"));
    assert!(
        json["message"]
            .as_str()
            .unwrap()
            .contains("invalid project")
    );
    assert!(!error.to_string().contains(TEST_API_KEY));
    support::assert_request(&request, "GET", "/issues/22.json?include=journals", None);
}

#[test]
fn metadata_errors_redact_api_key() {
    let response = MockResponse::error(422, format!(r#"{{"errors":["bad {TEST_API_KEY}"]}}"#));
    let (result, _) = one(response, |redmine| redmine.list_projects());
    let error = result.unwrap_err();
    assert!(!error.to_string().contains(TEST_API_KEY));
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap()
            .contains("[redacted]")
    );
}

#[test]
fn empty_redmine_http_errors_include_operation_and_status() {
    let (result, request) = one(MockResponse::error(403, ""), |redmine| {
        redmine.get_issue(23)
    });
    let error = result.unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 403);
    assert_eq!(json["operation"], "issue get");
    let message = json["message"].as_str().unwrap();
    assert!(message.contains("issue get"));
    assert!(message.contains("403"));
    assert!(!message.contains("Redmine returned an error"));
    support::assert_request(&request, "GET", "/issues/23.json?include=journals", None);
}

fn climb_statuses() -> String {
    serde_json::json!({
        "issue_statuses": [
            {"id": 1, "name": "New", "is_closed": false},
            {"id": 2, "name": "In Progress", "is_closed": false},
            {"id": 3, "name": "In Review", "is_closed": false},
            {"id": 4, "name": "Resolved", "is_closed": false},
            {"id": 37, "name": "Closed", "is_closed": true}
        ]
    })
    .to_string()
}

fn issue_with_named_status(id: u64, name: &str, closed: bool) -> String {
    serde_json::json!({
        "issue": {
            "id": id,
            "subject": "Title",
            "description": "Body",
            "status": {"name": name, "is_closed": closed},
            "journals": []
        }
    })
    .to_string()
}

#[test]
fn workflow_classification_drives_close_climb_to_success() {
    use crate::providers::redmine::model::{RedmineErrorKind, classify_redmine_error};
    let workflow = crate::providers::api::ForgejoError::Http {
        operation: "issue close".to_owned(),
        status: 422,
        message: "Status is invalid".to_owned(),
    };
    assert_eq!(
        classify_redmine_error(&workflow),
        RedmineErrorKind::WorkflowNotAllowed
    );
    // In Review climbs one step to Resolved, then the close PUT succeeds.
    // P2 (issue 649): the native open-child preflight runs first with an
    // empty page, so the close proceeds to the legacy direct PUT + climb.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(open_children_empty()),
        MockResponse::error(422, r#"{"errors":["Status is invalid"]}"#),
        MockResponse::ok(climb_statuses()),
        MockResponse::ok(issue_with_named_status(20, "In Review", false)),
        MockResponse::ok(climb_statuses()),
        MockResponse::ok(issue_with_named_status(20, "In Review", false)),
        MockResponse::ok(issue_with_named_status(20, "Resolved", false)),
        MockResponse::ok(issue_with_named_status(20, "Closed", true)),
    ]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let summary = redmine.close_issue(20).expect("climb must close");
    assert_eq!(summary.state, "closed");
    let seen = requests.recv().unwrap();
    assert_eq!(
        seen.len(),
        8,
        "preflight + direct + climb reads + step PUT + retry"
    );
    support::assert_request(&seen[0], "GET", "/issues.json", None);
    assert!(
        seen[0].contains("parent_id=20") && seen[0].contains("status_id=open"),
        "preflight must query native open children: {seen:?}"
    );
    assert!(
        !seen[0].contains("relations"),
        "preflight must never read relations: {seen:?}"
    );
    support::assert_request(&seen[1], "PUT", "/issues/20.json", None);
    assert!(seen[1].contains(r#""issue":{"status_id":37}"#));
    support::assert_request(&seen[6], "PUT", "/issues/20.json", None);
    assert!(seen[6].contains(r#""issue":{"status_id":4}"#));
    support::assert_request(&seen[7], "PUT", "/issues/20.json", None);
    assert!(seen[7].contains(r#""issue":{"status_id":37}"#));
    server.join().unwrap();
}

#[test]
fn close_climb_failure_returns_structured_forbidden_with_recovery() {
    // Direct close rejected, climb step rejected: structured Forbidden.
    // P2 (issue 649): empty preflight first, then the legacy sequence.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(open_children_empty()),
        MockResponse::error(422, r#"{"errors":["Status is invalid"]}"#),
        MockResponse::ok(climb_statuses()),
        MockResponse::ok(issue_with_named_status(20, "New", false)),
        MockResponse::ok(climb_statuses()),
        MockResponse::ok(issue_with_named_status(20, "New", false)),
        MockResponse::error(422, r#"{"errors":["Status is invalid"]}"#),
    ]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let error = redmine.close_issue(20).unwrap_err();
    let json = error.json();
    assert_eq!(json["operation"], "issue close");
    let message = json["message"].as_str().unwrap();
    for expected in [
        "'New'",
        "'Closed'",
        "allowed_next=[In Progress, Cancelled]",
        "phasegent/canonical-phase-workflow@v1",
        "status next 20",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
    let seen = requests.recv().unwrap();
    assert_eq!(
        seen.len(),
        7,
        "preflight + direct + climb reads + failed step"
    );
    support::assert_request(&seen[0], "GET", "/issues.json", None);
    assert!(seen[0].contains("parent_id=20") && seen[0].contains("status_id=open"));
    server.join().unwrap();
}

fn issue_with_status_id(id: u64, status_id: u64, name: &str) -> String {
    serde_json::json!({
        "issue": {
            "id": id,
            "subject": "Title",
            "description": "Body",
            "status": {"id": status_id, "name": name},
            "journals": []
        }
    })
    .to_string()
}

/// Empty native open-children page for the `parent_id` + `status_id=open`
/// preflight: the issue has no known open child.
fn open_children_empty() -> String {
    serde_json::json!({"issues": [], "total_count": 0}).to_string()
}

/// Native open-children page: each child carries id/subject/open status
/// so the diagnostic can list bounded identity/title/status.
fn open_children_list(children: &[(u64, &str, &str)]) -> String {
    serde_json::json!({
        "issues": children.iter().map(|(id, subject, status)| serde_json::json!({
            "id": id,
            "subject": subject,
            "description": "Child body",
            "status": {"id": 1, "name": status, "is_closed": false},
        })).collect::<Vec<_>>(),
        "total_count": children.len(),
    })
    .to_string()
}

#[test]
fn silent_200_close_mismatch_climbs_to_success() {
    // Dogfood `issue close 443`: the direct PUT returns 200 but the
    // observed status stays New. The mismatch classifies as
    // WorkflowNotAllowed, so close climbs New -> In Progress ->
    // In Review -> Resolved and retries the close PUT.
    // P2 (issue 649): empty preflight first, then the legacy sequence.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(open_children_empty()),
        MockResponse::ok(issue_with_status_id(20, 1, "New")),
        MockResponse::ok(climb_statuses()),
        MockResponse::ok(issue_with_named_status(20, "New", false)),
        MockResponse::ok(climb_statuses()),
        MockResponse::ok(issue_with_named_status(20, "New", false)),
        MockResponse::ok(issue_with_named_status(20, "In Progress", false)),
        MockResponse::ok(climb_statuses()),
        MockResponse::ok(issue_with_named_status(20, "In Progress", false)),
        MockResponse::ok(issue_with_named_status(20, "In Review", false)),
        MockResponse::ok(climb_statuses()),
        MockResponse::ok(issue_with_named_status(20, "In Review", false)),
        MockResponse::ok(issue_with_named_status(20, "Resolved", false)),
        MockResponse::ok(issue_with_named_status(20, "Closed", true)),
    ]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let summary = redmine.close_issue(20).expect("mismatch must climb");
    assert_eq!(summary.state, "closed");
    let seen = requests.recv().unwrap();
    assert_eq!(
        seen.len(),
        14,
        "preflight + mismatch PUT + climb reads + steps + retry"
    );
    support::assert_request(&seen[0], "GET", "/issues.json", None);
    assert!(seen[0].contains("parent_id=20") && seen[0].contains("status_id=open"));
    support::assert_request(&seen[1], "PUT", "/issues/20.json", None);
    assert!(seen[1].contains(r#""issue":{"status_id":37}"#));
    support::assert_request(&seen[6], "PUT", "/issues/20.json", None);
    assert!(seen[6].contains(r#""issue":{"status_id":2}"#));
    support::assert_request(&seen[9], "PUT", "/issues/20.json", None);
    assert!(seen[9].contains(r#""issue":{"status_id":3}"#));
    support::assert_request(&seen[12], "PUT", "/issues/20.json", None);
    assert!(seen[12].contains(r#""issue":{"status_id":4}"#));
    support::assert_request(&seen[13], "PUT", "/issues/20.json", None);
    assert!(seen[13].contains(r#""issue":{"status_id":37}"#));
    server.join().unwrap();
}

#[test]
fn close_stale_resolved_reports_unconfirmed_noop_not_workflow_refusal() {
    // Issue 640: the issue is already `Resolved` and the direct
    // `PUT close_id` returns `200 OK` with the unchanged `Resolved`
    // status. The canonical policy allows `Resolved -> Closed`, so the
    // `Forbidden`-style "not allowed" wording would contradict
    // `allowed_next=[Closed, ...]`. The close must fail as an
    // unconfirmed server-side no-op with workflow/permission checks,
    // without climbing (no steps from `Resolved`) and without
    // weakening verification.
    // P2 (issue 649): the native open-child preflight runs first.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(open_children_empty()),
        MockResponse::ok(issue_with_status_id(20, 4, "Resolved")),
        MockResponse::ok(climb_statuses()),
        MockResponse::ok(issue_with_status_id(20, 4, "Resolved")),
    ]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let error = redmine.close_issue(20).unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "request");
    assert_eq!(json["operation"], "issue close");
    let message = json["message"].as_str().unwrap();
    for expected in [
        "close not confirmed",
        "unconfirmed server-side no-op",
        "'Resolved'",
        "'Closed'",
        "37",
        "not evidence the close status id is wrong",
        "workflow",
        "Edit issues",
        "required fields",
        "author/assignee",
        "phasegent/canonical-phase-workflow@v1",
        "allowed_next=[Closed, In Progress]",
        "status next 20",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
    for misleading in [
        "close rejected by server workflow",
        "is not allowed by the Redmine server workflow",
    ] {
        assert!(
            !message.contains(misleading),
            "misleading diagnostic {misleading} must not appear: {message}"
        );
    }
    let seen = requests.recv().unwrap();
    assert_eq!(
        seen.len(),
        4,
        "preflight + direct PUT + climb status list + climb current-issue GET; no climb steps from Resolved: {seen:?}"
    );
    support::assert_request(&seen[0], "GET", "/issues.json", None);
    assert!(seen[0].contains("parent_id=20") && seen[0].contains("status_id=open"));
    support::assert_request(&seen[1], "PUT", "/issues/20.json", None);
    assert!(seen[1].contains(r#""issue":{"status_id":37}"#));
    support::assert_request(&seen[2], "GET", "/issue_statuses.json", None);
    support::assert_request(&seen[3], "GET", "/issues/20.json", None);
    server.join().unwrap();
}

#[test]
fn close_preserves_non_workflow_refusal_without_climb() {
    // Empty 403 has no workflow marker: preflight (empty) + single PUT,
    // legacy shape. The preflight proves the empty-child success path
    // reaches the PUT; the refusal itself never climbs.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(open_children_empty()),
        MockResponse::error(403, ""),
    ]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let error = redmine.close_issue(20).unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 403);
    assert!(!json["message"].as_str().unwrap().contains("allowed_next"));
    let seen = requests.recv().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "empty preflight + non-workflow refusal must not climb"
    );
    support::assert_request(&seen[0], "GET", "/issues.json", None);
    assert!(seen[0].contains("parent_id=20") && seen[0].contains("status_id=open"));
    support::assert_request(&seen[1], "PUT", "/issues/20.json", None);
    server.join().unwrap();
}

#[test]
fn close_blocked_when_native_open_children_exist() {
    // P2 (issue 649): known open children fail before any parent PUT.
    // Preflight children GET + parent status GET, then the structured
    // diagnostic; no PUT is ever sent and no cascade occurs.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(open_children_list(&[
            (641, "Child alpha", "New"),
            (642, "Child beta", "In Progress"),
        ])),
        MockResponse::ok(issue_with_status_id(20, 4, "Resolved")),
    ]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let error = redmine.close_issue(20).unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "request");
    assert_eq!(json["operation"], "issue close");
    let message = json["message"].as_str().unwrap();
    for expected in [
        "cannot close issue 20",
        "2 open child",
        "#641",
        "Child alpha",
        "New",
        "#642",
        "Child beta",
        "In Progress",
        "close each child individually",
        "never cascade",
        "sent no parent status update",
        "status next 20",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
    let seen = requests.recv().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "children preflight + parent status; no parent PUT"
    );
    support::assert_request(&seen[0], "GET", "/issues.json", None);
    assert!(seen[0].contains("parent_id=20") && seen[0].contains("status_id=open"));
    assert!(
        !seen[0].contains("relations"),
        "preflight must never read relations"
    );
    support::assert_request(&seen[1], "GET", "/issues/20.json", None);
    assert!(
        seen.iter().all(|request| !request.starts_with("PUT")),
        "no parent PUT may fire when open children are known: {seen:?}"
    );
    server.join().unwrap();
}

#[test]
fn status_to_closed_blocked_when_native_open_children_exist() {
    // P2 (issue 649): the explicit status-to-Closed path shares the
    // preflight with identical query shape and operation-preserving
    // `issue status update` contract. No parent PUT fires.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(open_children_list(&[(641, "Child alpha", "New")])),
        MockResponse::ok(issue_with_status_id(20, 4, "Resolved")),
    ]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let error = redmine.set_issue_status(20, 37).unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "request");
    assert_eq!(json["operation"], "issue status update");
    let message = json["message"].as_str().unwrap();
    for expected in [
        "cannot close issue 20",
        "#641",
        "Child alpha",
        "close each child individually",
        "never cascade",
        "sent no parent status update",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
    let seen = requests.recv().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "children preflight + parent status; no parent PUT"
    );
    support::assert_request(&seen[0], "GET", "/issues.json", None);
    assert!(seen[0].contains("parent_id=20") && seen[0].contains("status_id=open"));
    assert!(
        seen.iter().all(|request| !request.starts_with("PUT")),
        "no parent PUT may fire on the explicit Closed path: {seen:?}"
    );
    server.join().unwrap();
}

#[test]
fn open_children_diagnostic_is_bounded() {
    // P2 (issue 649): twelve open children list ten plus `+2 more`.
    let children: Vec<(u64, String, String)> = (1..=12)
        .map(|index| (600 + index, format!("Child {index}"), "New".to_owned()))
        .collect();
    let borrowed: Vec<(u64, &str, &str)> = children
        .iter()
        .map(|(id, subject, status)| (*id, subject.as_str(), status.as_str()))
        .collect();
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(open_children_list(&borrowed)),
        MockResponse::ok(issue_with_status_id(20, 4, "Resolved")),
    ]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let error = redmine.close_issue(20).unwrap_err();
    let message = error.json()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("12 open child"),
        "count must be exact: {message}"
    );
    assert!(
        message.contains("+2 more"),
        "remainder must be reported: {message}"
    );
    assert!(
        message.contains("#601") && message.contains("#610"),
        "first ten listed: {message}"
    );
    assert!(
        !message.contains("#611") && !message.contains("#612"),
        "tail hidden: {message}"
    );
    let seen = requests.recv().unwrap();
    assert!(seen.iter().all(|request| !request.starts_with("PUT")));
    server.join().unwrap();
}

#[test]
fn already_closed_parent_with_open_children_still_closes() {
    // P2 (issue 649): an already-closed parent keeps its idempotent
    // close even when open children are known.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(open_children_list(&[(641, "Child alpha", "New")])),
        MockResponse::ok(issue_with_status_id(20, 37, "Closed")),
        MockResponse::ok(issue_with_status_id(20, 37, "Closed")),
    ]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let summary = redmine
        .close_issue(20)
        .expect("already-closed parent must close");
    assert_eq!(summary.number, 20);
    let seen = requests.recv().unwrap();
    assert_eq!(seen.len(), 3, "preflight + parent status + direct PUT");
    support::assert_request(&seen[2], "PUT", "/issues/20.json", None);
    server.join().unwrap();
}

#[test]
fn non_close_status_set_skips_preflight() {
    // Other statuses keep the legacy single-PUT shape: no children GET.
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_with_status_id(
        20,
        2,
        "In Progress",
    ))]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let summary = redmine
        .set_issue_status(20, 2)
        .expect("non-close set must succeed");
    assert_eq!(summary.number, 20);
    let seen = requests.recv().unwrap();
    assert_eq!(seen.len(), 1, "non-close set must not preflight");
    support::assert_request(&seen[0], "PUT", "/issues/20.json", None);
    assert!(seen[0].contains(r#""issue":{"status_id":2}"#));
    server.join().unwrap();
}
