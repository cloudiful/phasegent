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
fn time_entry_activity_selection_is_exact_preferred_or_singular_default() {
    let activities = vec![
        RedmineTimeEntryActivity {
            id: 10,
            name: "Design".to_owned(),
            is_default: true,
        },
        RedmineTimeEntryActivity {
            id: 11,
            name: "Development".to_owned(),
            is_default: false,
        },
    ];
    assert_eq!(
        RedmineProvider::select_time_entry_activity(&activities)
            .unwrap()
            .id,
        11,
        "exact Development must beat the default"
    );

    let ambiguous = vec![
        RedmineTimeEntryActivity {
            id: 1,
            name: "AI automation".to_owned(),
            is_default: true,
        },
        RedmineTimeEntryActivity {
            id: 2,
            name: "AI automation".to_owned(),
            is_default: false,
        },
    ];
    let error = RedmineProvider::select_time_entry_activity(&ambiguous).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(error.to_string().contains("ambiguous"));

    let multiple_defaults = vec![
        RedmineTimeEntryActivity {
            id: 3,
            name: "Design".to_owned(),
            is_default: true,
        },
        RedmineTimeEntryActivity {
            id: 4,
            name: "Testing".to_owned(),
            is_default: true,
        },
    ];
    let error = RedmineProvider::select_time_entry_activity(&multiple_defaults).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(error.to_string().contains("multiple default"));

    let no_candidate = vec![RedmineTimeEntryActivity {
        id: 5,
        name: "Design".to_owned(),
        is_default: false,
    }];
    let error = RedmineProvider::select_time_entry_activity(&no_candidate).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
}

#[test]
fn time_entry_activity_list_uses_redmine_enumeration_endpoint() {
    let (result, request) = one(
        MockResponse::ok(time_entry_activities(&[
            (1, "Development", false),
            (2, "Design", true),
        ])),
        |redmine| redmine.list_time_entry_activities(),
    );
    assert_eq!(result.unwrap()[1].id, 2);
    support::assert_request(
        &request,
        "GET",
        "/enumerations/time_entry_activities.json",
        None,
    );
}

#[test]
fn time_entry_create_sends_exact_projection_and_decodes_201() {
    let body = time_entry_response(77, 28, 9, 0.02, "marker", "2026-08-25");
    let (result, request) = one(MockResponse::status(201, body), |redmine| {
        redmine.create_time_entry(28, 0.02, "2026-08-25", 9, "marker")
    });
    let entry = result.unwrap().expect("201 should contain a time entry");
    assert_eq!(entry.id, 77);
    support::assert_request(&request, "POST", "/time_entries.json", None);
    assert!(request.contains(r#""time_entry":{"issue_id":28,"hours":0.02,"spent_on":"2026-08-25","activity_id":9,"comments":"marker"}"#));
}

#[test]
fn time_entry_create_accepts_204_and_empty_201_without_decoding_error() {
    let (result, request) = one(MockResponse::status(204, ""), |redmine| {
        redmine.create_time_entry(28, 0.01, "2026-08-25", 9, "marker")
    });
    assert!(result.unwrap().is_none());
    support::assert_request(&request, "POST", "/time_entries.json", None);

    let (result, _) = one(MockResponse::status(201, "{}"), |redmine| {
        redmine.create_time_entry(28, 0.01, "2026-08-25", 9, "marker")
    });
    assert!(result.unwrap().is_none());

    let (result, _) = one(MockResponse::status(204, "{}"), |redmine| {
        redmine.create_time_entry(28, 0.01, "2026-08-25", 9, "marker")
    });
    assert!(result.unwrap().is_none());
}

#[test]
fn time_entry_list_reconciles_the_stable_run_marker() {
    let comments = "phasegent timer run_id=run-1";
    let body = time_entry_collection(&[(901, 28, 9, 0.02, comments, "2026-08-25")]);
    let (result, request) = one(MockResponse::ok(body), |redmine| {
        redmine.find_time_entry_by_comments(28, "2026-08-25", comments)
    });
    let entry = result.unwrap().expect("marker should reconcile");
    assert_eq!(entry.id, 901);
    support::assert_request(&request, "GET", "/time_entries.json?", None);
    assert!(request.contains("issue_id=28"));
    assert!(request.contains("from=2026-08-25"));
    assert!(request.contains("to=2026-08-25"));
}
