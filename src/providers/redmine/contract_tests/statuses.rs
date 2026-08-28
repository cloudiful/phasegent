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
fn issue_status_list_decodes_redmine_wrapper() {
    let (result, request) = one(
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [
                    {"id": 1, "name": "New", "is_closed": false},
                    {"id": 5, "name": "Closed", "is_closed": true}
                ]
            })
            .to_string(),
        ),
        |redmine| redmine.list_issue_statuses(),
    );
    let statuses = result.unwrap();
    assert_eq!(
        statuses.iter().map(|status| status.id).collect::<Vec<_>>(),
        [1, 5]
    );
    assert!(statuses[1].is_closed);
    support::assert_request(&request, "GET", "/issue_statuses.json", None);
}

#[test]
fn tracker_list_decodes_redmine_wrapper() {
    let (result, request) = one(
        MockResponse::ok(
            serde_json::json!({
                "trackers": [
                    {"id": 1, "name": "Bug"},
                    {"id": 2, "name": "Feature"}
                ]
            })
            .to_string(),
        ),
        |redmine| redmine.list_trackers(),
    );
    let trackers = result.unwrap();
    assert_eq!(
        trackers
            .iter()
            .map(|tracker| (tracker.id, tracker.name.as_str()))
            .collect::<Vec<_>>(),
        [(1, "Bug"), (2, "Feature")]
    );
    support::assert_request(&request, "GET", "/trackers.json", None);
}

#[test]
fn status_and_tracker_selection_validate_name_id_and_ambiguity() {
    let statuses = vec![
        RedmineIssueStatus {
            id: 1,
            name: "New".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 2,
            name: "In Progress".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 7,
            name: "New".to_owned(),
            is_closed: false,
        },
    ];
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "In Progress")
            .unwrap()
            .id,
        2
    );
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "7")
            .unwrap()
            .id,
        7
    );
    // A duplicate name is ambiguous even when one candidate carries the id.
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "New")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "Blocked")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
    assert_eq!(
        RedmineProvider::select_status_by_value(&statuses, "0")
            .unwrap_err()
            .json()["kind"],
        "config"
    );

    let trackers = vec![
        crate::providers::redmine::model::RedmineTracker {
            id: 1,
            name: "Bug".to_owned(),
        },
        crate::providers::redmine::model::RedmineTracker {
            id: 2,
            name: "Feature".to_owned(),
        },
    ];
    assert_eq!(
        RedmineProvider::select_tracker(&trackers, "Bug")
            .unwrap()
            .id,
        1
    );
    assert_eq!(
        RedmineProvider::select_tracker(&trackers, "2").unwrap().id,
        2
    );
    assert_eq!(
        RedmineProvider::select_tracker(&trackers, "Task")
            .unwrap_err()
            .json()["kind"],
        "config"
    );
}
