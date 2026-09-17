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
    let (base, requests, server) = sequence(vec![
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
    assert_eq!(seen.len(), 7, "direct + climb reads + step PUT + retry");
    support::assert_request(&seen[0], "PUT", "/issues/20.json", None);
    assert!(seen[0].contains(r#""issue":{"status_id":37}"#));
    support::assert_request(&seen[5], "PUT", "/issues/20.json", None);
    assert!(seen[5].contains(r#""issue":{"status_id":4}"#));
    support::assert_request(&seen[6], "PUT", "/issues/20.json", None);
    assert!(seen[6].contains(r#""issue":{"status_id":37}"#));
    server.join().unwrap();
}

#[test]
fn close_climb_failure_returns_structured_forbidden_with_recovery() {
    // Direct close rejected, climb step rejected: structured Forbidden.
    let (base, requests, server) = sequence(vec![
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
    assert_eq!(seen.len(), 6, "direct + climb reads + failed step");
    server.join().unwrap();
}

#[test]
fn close_preserves_non_workflow_refusal_without_climb() {
    // Empty 403 has no workflow marker: single PUT, legacy shape.
    let (base, requests, server) = sequence(vec![MockResponse::error(403, "")]);
    let redmine =
        RedmineProvider::new(RedmineConfig::new(base, "42", 37), TEST_API_KEY.to_owned()).unwrap();
    let error = redmine.close_issue(20).unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "http");
    assert_eq!(json["status"], 403);
    assert!(!json["message"].as_str().unwrap().contains("allowed_next"));
    let seen = requests.recv().unwrap();
    assert_eq!(seen.len(), 1, "non-workflow refusal must not climb");
    server.join().unwrap();
}
