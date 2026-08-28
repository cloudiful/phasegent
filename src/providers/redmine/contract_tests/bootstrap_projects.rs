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
use crate::ci_model::CiRunsFilter;
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
fn bootstrap_derives_identifier_with_owner_and_redmine_normalization() {
    assert_eq!(
        crate::remote::redmine_identifier("Acme/Workflow Repo").unwrap(),
        "acme-workflow-repo"
    );
    assert_eq!(
        crate::remote::redmine_identifier("Owner.Name/Repo+One").unwrap(),
        "owner-name-repo-one"
    );
    assert_eq!(
        crate::remote::redmine_identifier("!owner/repo").unwrap(),
        "owner-repo"
    );
    assert_eq!(
        crate::remote::redmine_identifier("123owner/repo").unwrap(),
        "wf-123owner-repo"
    );
}

#[test]
fn bootstrap_project_lookup_is_exact_and_404_means_missing() {
    let response = support::project_response(44, "Workflow", "acme-repo", "description");
    let (result, request) = one(MockResponse::ok(response), |redmine| {
        redmine.find_project("acme-repo")
    });
    assert_eq!(result.unwrap().unwrap().id, 44);
    support::assert_request(&request, "GET", "/projects/acme-repo.json", None);

    let (result, _) = one(
        MockResponse::ok(support::project_response(
            45,
            "Workflow",
            "other-project",
            "description",
        )),
        |redmine| redmine.find_project("acme-repo"),
    );
    assert!(result.unwrap().is_none());

    let (result, request) = one(
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        |redmine| redmine.find_project("missing-project"),
    );
    assert!(result.unwrap().is_none());
    support::assert_request(&request, "GET", "/projects/missing-project.json", None);
}

#[test]
fn bootstrap_found_project_selects_status_without_creating() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::project_response(
            44,
            "Workflow",
            "acme-repo",
            "description",
        )),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [
                    {"id": 1, "name": "New", "is_closed": false},
                    {"id": 5, "name": "Closed", "is_closed": true}
                ]
            })
            .to_string(),
        ),
    ]);
    let redmine = provider(base);
    let bootstrap = redmine
        .bootstrap_project("acme/repo", "acme-repo", None, None)
        .unwrap();
    assert_eq!(bootstrap.project.id, 44);
    assert_eq!(bootstrap.close_status.id, 5);
    assert!(!bootstrap.created);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    support::assert_request(&requests[0], "GET", "/projects/acme-repo.json", None);
    support::assert_request(&requests[1], "GET", "/issue_statuses.json", None);
    server.join().unwrap();
}

#[test]
fn bootstrap_missing_project_creates_automatically_without_confirmation() {
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
            })
            .to_string(),
        ),
        MockResponse::ok(support::project_response(
            44,
            "acme/repo",
            "acme-repo",
            "Workflow issues for acme/repo",
        )),
    ]);
    let redmine = provider(base);
    let bootstrap = redmine
        .bootstrap_project("acme/repo", "acme-repo", None, None)
        .unwrap();
    assert_eq!(bootstrap.project.id, 44);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /projects/acme-repo.json"));
    support::assert_request(&requests[2], "POST", "/projects.json", None);
    assert!(requests[2].contains(r#""is_public":false"#));
    server.join().unwrap();
}

#[test]
fn bootstrap_missing_project_creation_is_private() {
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
            })
            .to_string(),
        ),
        MockResponse::ok(support::project_response(
            44,
            "acme/repo",
            "acme-repo",
            "Workflow issues for acme/repo",
        )),
    ]);
    let redmine = provider(base);
    let bootstrap = redmine
        .bootstrap_project("acme/repo", "acme-repo", None, None)
        .unwrap();
    assert_eq!(bootstrap.project.id, 44);
    assert_eq!(bootstrap.close_status.id, 5);
    assert!(bootstrap.created);
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    support::assert_request(&requests[2], "POST", "/projects.json", None);
    assert!(requests[2].contains(r#""identifier":"acme-repo""#));
    assert!(requests[2].contains(r#""is_public":false"#));
    server.join().unwrap();
}
