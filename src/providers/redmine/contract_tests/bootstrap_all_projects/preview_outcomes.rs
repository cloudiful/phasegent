use super::{projects_response, service_roles_payload};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::providers::redmine::contract_tests::bootstrap_auto::{
    seed_admin_only, temp_db, user_list_with,
};
use crate::providers::redmine::contract_tests::support::{
    MockResponse, membership_collection_page, role_collection, sequence,
};
use std::fs;

const ALL_USERS: [(u64, &str); 4] = [
    (11, "phasegent-orchestrator"),
    (22, "phasegent-executor"),
    (33, "phasegent-reviewer"),
    (44, "phasegent-explore"),
];

#[test]
fn preview_marks_additive_membership_update_without_writing() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-preview-additive-update");
    let (base, requests, server) = sequence(vec![
        projects_response(1, 100, &[(44, "Workflow", "workflow")]),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(service_roles_payload()),
        MockResponse::ok(membership_collection_page(
            1,
            100,
            Some((55, 22, "phasegent-executor", vec![3])),
        )),
    ]);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, true).unwrap();
    let executor = report.projects[0]
        .memberships
        .iter()
        .find(|membership| membership.role == "executor")
        .unwrap();
    assert!(report.ready);
    assert_eq!(executor.status, "updated");
    assert_eq!(report.summary.updated, 1);
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 7, "{reqs:?}");
    assert!(
        reqs.iter()
            .all(|request| !request.starts_with("POST") && !request.starts_with("PUT"))
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn preview_reports_ambiguous_role_names_without_guessing() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-preview-ambiguous-role");
    let (base, requests, server) = sequence(vec![
        projects_response(1, 100, &[(44, "Workflow", "workflow")]),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
            (6, "Reporter"),
        ])),
        MockResponse::ok(
            crate::providers::redmine::contract_tests::support::membership_collection(None),
        ),
    ]);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, true).unwrap();
    assert!(!report.ready);
    assert_eq!(report.summary.warnings, 2);
    for role in ["reviewer", "explore"] {
        let membership = report.projects[0]
            .memberships
            .iter()
            .find(|membership| membership.role == role)
            .unwrap();
        assert_eq!(membership.status, "warning");
        assert_eq!(membership.warning.as_deref(), Some("role is ambiguous"));
    }
    let reqs = requests.recv().unwrap();
    assert!(
        reqs.iter()
            .all(|request| !request.starts_with("POST") && !request.starts_with("PUT"))
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn incomplete_membership_page_blocks_only_that_project_report() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-preview-incomplete-membership");
    let (base, requests, server) = sequence(vec![
        projects_response(1, 100, &[(44, "Workflow", "workflow")]),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(service_roles_payload()),
        MockResponse::ok(membership_collection_page(2, 100, None)),
    ]);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, true).unwrap();
    assert!(!report.ready);
    assert_eq!(
        report.projects[0].error.as_ref().unwrap()["kind"],
        "pagination"
    );
    assert!(
        report.projects[0]
            .memberships
            .iter()
            .all(|membership| membership.status == "blocked")
    );
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 7, "{reqs:?}");
    assert!(
        reqs.iter()
            .all(|request| !request.starts_with("POST") && !request.starts_with("PUT"))
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
