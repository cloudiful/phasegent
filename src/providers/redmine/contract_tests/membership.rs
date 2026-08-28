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
fn current_user_decodes_user_payload_from_users_current() {
    let (result, request) = one(
        MockResponse::ok(support::current_user_response(101, "orchestrator")),
        |redmine| redmine.current_user(),
    );
    let user = result.unwrap();
    assert_eq!(user.id, 101);
    assert_eq!(user.login, "orchestrator");
    support::assert_request(&request, "GET", "/users/current.json", None);
}

#[test]
fn user_membership_existing_role_is_not_changed() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::role_collection(&[(9, "Developer")])),
        MockResponse::ok(support::membership_collection(Some((
            55,
            7,
            "executor",
            vec![9],
        )))),
    ]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "existing");
    assert_eq!(result.user_id, 7);
    assert_eq!(result.user_login, "executor");
    assert_eq!(result.role_id, 9);
    assert_eq!(result.role_name, "Developer");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    support::assert_request(&requests[0], "GET", "/roles.json", None);
    support::assert_request(&requests[1], "GET", "/projects/42/memberships.json", None);
    server.join().unwrap();
}

#[test]
fn user_membership_missing_role_is_a_warning_without_membership_write() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(support::role_collection(&[(
        3, "Reporter",
    )]))]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "warning");
    assert_eq!(result.user_id, 7);
    assert_eq!(result.user_login, "executor");
    assert_eq!(result.role_name, "Developer");
    let warning = result.warning.expect("missing warning text");
    assert!(
        warning.contains("role 'Developer'"),
        "unexpected warning: {warning}"
    );
    assert_eq!(requests.recv().unwrap().len(), 1);
    server.join().unwrap();
}

#[test]
fn user_membership_missing_entry_is_added_with_selected_role() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::role_collection(&[(9, "Developer")])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
    ]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "added");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    support::assert_request(&requests[2], "POST", "/projects/42/memberships.json", None);
    assert!(requests[2].contains(r#""user_id":7,"role_ids":[9]"#));
    assert!(!requests[2].contains("group_id"));
    server.join().unwrap();
}

#[test]
fn user_membership_existing_entry_adds_missing_role_without_dropping_others() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::role_collection(&[(9, "Developer")])),
        MockResponse::ok(support::membership_collection(Some((
            55,
            7,
            "executor",
            vec![3],
        )))),
        MockResponse::ok("{}"),
    ]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "updated");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    support::assert_request(&requests[2], "PUT", "/memberships/55.json", None);
    // Role list is sorted ascending in update payloads and never overwrites
    // the unrelated Reporter role already on the membership.
    assert!(requests[2].contains(r#""role_ids":[3,9]"#));
    server.join().unwrap();
}

#[test]
fn user_membership_reconciliation_finds_roles_and_memberships_on_later_pages() {
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(support::role_collection_page(2, 1, &[(3, "Reporter")])),
        MockResponse::ok(support::role_collection_page(2, 1, &[(9, "Developer")])),
        MockResponse::ok(support::membership_collection_page(
            2,
            1,
            Some((20, 7, "executor", vec![9])),
        )),
        MockResponse::ok(support::membership_collection_page(
            2,
            1,
            Some((55, 7, "executor", vec![9])),
        )),
    ]);
    let redmine = provider(base);
    let user = support::user_from_response(&support::current_user_response(7, "executor"));
    let result = redmine
        .ensure_user_membership(42, &user, "Developer")
        .unwrap();
    assert_eq!(result.status, "existing");
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].contains("limit=100"));
    assert!(requests[0].contains("offset=0"));
    assert!(requests[1].contains("offset=1"));
    assert!(requests[2].contains("offset=0"));
    assert!(requests[3].contains("offset=1"));
    server.join().unwrap();
}
