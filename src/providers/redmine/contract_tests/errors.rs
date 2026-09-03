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
