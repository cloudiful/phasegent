use super::{projects_response, service_roles_payload};
use crate::auth;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::providers::redmine::contract_tests::bootstrap_auto::{
    SERVICE_ROLES, membership_added, seed_admin_only, temp_db,
};
use crate::providers::redmine::contract_tests::support::{
    MockResponse, membership_collection_page, role_collection, sequence,
};
use std::fs;

fn persist_identities(storage: &crate::infra::storage::Storage) {
    for (role, id, login) in SERVICE_ROLES {
        storage
            .save_credential(role, "redmine", &format!("{}-key", role.as_str()))
            .unwrap();
        auth::save_redmine_user(role, id, login, storage).unwrap();
    }
}

#[test]
fn project_keeps_first_structured_error_and_counts_multiple_failures_once() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-apply-multiple-errors");
    let mut responses = vec![projects_response(
        2,
        100,
        &[(11, "First", "first"), (22, "Second", "second")],
    )];
    persist_identities(&storage);
    responses.extend([
        MockResponse::ok(service_roles_payload()),
        MockResponse::ok(membership_collection_page(2, 100, None)),
        MockResponse::ok(service_roles_payload()),
        MockResponse::status(403, "{}"),
    ]);
    for _ in 0..6 {
        responses.extend(membership_added());
    }
    let (base, requests, server) = sequence(responses);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, false).unwrap();
    assert!(!report.ready);
    assert!(!report.projects[0].ready);
    assert!(report.projects[1].ready);
    let error = report.projects[0].error.as_ref().unwrap();
    assert_eq!(error["kind"], "pagination");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("empty page"))
    );
    assert_eq!(report.projects[0].memberships[0].status, "blocked");
    assert!(
        report.projects[0].memberships[0]
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("empty page"))
    );
    assert_eq!(report.projects[0].memberships[1].status, "blocked");
    assert!(
        report.projects[0].memberships[1]
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("Redmine returned an error"))
    );
    assert_eq!(report.summary.added, 6);
    assert_eq!(report.summary.warnings, 2);
    assert_eq!(report.summary.errors, 1);

    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 23, "{reqs:?}");
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn unresolved_role_warning_does_not_increment_project_errors() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-apply-role-warning");
    persist_identities(&storage);
    let mut responses = vec![projects_response(1, 100, &[(11, "First", "first")])];
    responses.extend(membership_added());
    responses.extend(membership_added());
    responses.push(MockResponse::ok(role_collection(&[
        (3, "Maintainer"),
        (4, "Developer"),
    ])));
    responses.extend(membership_added());
    let (base, requests, server) = sequence(responses);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, false).unwrap();
    assert!(!report.ready);
    assert!(report.projects[0].error.is_none());
    let reviewer = &report.projects[0].memberships[2];
    assert_eq!(reviewer.status, "warning");
    assert!(
        reviewer
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("role was not found"))
    );
    assert_eq!(report.summary.added, 3);
    assert_eq!(report.summary.warnings, 1);
    assert_eq!(report.summary.errors, 0);

    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 11, "{reqs:?}");
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
