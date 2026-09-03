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
fn project_list_uses_redmine_collection_wrapper_and_pagination_params() {
    let response = support::project_collection(1, 100, &[(41, "Workflow", "workflow")]);
    let (result, request) = one(MockResponse::ok(response), |redmine| {
        redmine.list_projects()
    });
    let projects = result.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, 41);
    assert_eq!(projects[0].identifier, "workflow");
    assert_eq!(projects[0].description, "description");
    assert_eq!(
        serde_json::to_value(&projects[0]).unwrap()["description"],
        "description"
    );
    support::assert_request(&request, "GET", "/projects.json?", None);
    assert!(request.contains("limit=100"));
    assert!(request.contains("offset=0"));
}

#[test]
fn project_list_decodes_null_description_as_empty_string() {
    let response = serde_json::json!({
        "total_count": 1,
        "limit": 100,
        "projects": [{
            "id": 41,
            "name": "Workflow",
            "identifier": "workflow",
            "description": null,
            "is_public": false,
            "inherit_members": false,
        }]
    })
    .to_string();
    let created_response = serde_json::json!({
        "project": {
            "id": 43,
            "name": "Created",
            "identifier": "created",
            "description": null,
            "is_public": false,
            "inherit_members": false,
        }
    })
    .to_string();
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(response),
        MockResponse::ok(created_response),
    ]);
    let redmine = provider(base);
    let projects = redmine.list_projects().unwrap();
    assert_eq!(projects[0].description, "");
    let serialized = serde_json::to_value(&projects[0]).unwrap();
    assert!(!serialized.as_object().unwrap().contains_key("description"));
    let created = redmine.create_project("Created", "created", None).unwrap();
    assert_eq!(created.description, "");
    let requests = requests.recv().unwrap();
    support::assert_request(&requests[0], "GET", "/projects.json?", None);
    support::assert_request(&requests[1], "POST", "/projects.json", None);
    server.join().unwrap();
}

#[test]
fn project_create_wraps_fields_and_does_not_change_configured_project() {
    let response = support::project_response(43, "Created", "created", "Created project");
    let (result, request) = one(MockResponse::ok(response), |redmine| {
        let result = redmine.create_project("Created", "created", Some("Created project"));
        assert_eq!(redmine.config.project_id.as_deref(), Some("42"));
        result
    });
    let project = result.unwrap();
    assert_eq!(project.id, 43);
    support::assert_request(&request, "POST", "/projects.json", None);
    // Bootstrap enables the `repository` module on creation so the mirror
    // plugin can attach the Git repository without a separate PUT.
    assert!(request.contains(
        r#""project":{"name":"Created","identifier":"created","is_public":false,"description":"Created project","enabled_modules":[{"name":"repository"}]}"#
    ));
}
