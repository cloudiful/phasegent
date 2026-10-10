use super::{projects_response, service_roles_payload};
use crate::auth;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::providers::redmine::contract_tests::bootstrap_auto::{
    SERVICE_ROLES, create_role, membership_added, seed_admin_only, temp_db,
};
use crate::providers::redmine::contract_tests::support::{MockResponse, sequence};
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
fn apply_provisions_once_across_the_complete_multi_page_project_set() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-apply-multi-project");
    let mut responses = vec![
        projects_response(2, 1, &[(22, "Second", "second")]),
        projects_response(2, 1, &[(11, "First", "first")]),
    ];
    for (role, id, login) in SERVICE_ROLES {
        responses.extend(create_role(id, login, &format!("{}-key", role.as_str())));
    }
    for _ in 0..8 {
        responses.extend(membership_added());
    }
    let (base, requests, server) = sequence(responses);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, false).unwrap();
    assert!(report.ready);
    assert_eq!(report.summary.projects, 2);
    assert_eq!(report.summary.added, 8);
    assert_eq!(
        report
            .projects
            .iter()
            .map(|project| project.project_id)
            .collect::<Vec<_>>(),
        [11, 22]
    );
    let reqs = requests.recv().unwrap();
    assert_eq!(
        reqs.iter()
            .filter(|request| request.starts_with("POST /users.json"))
            .count(),
        4
    );
    assert_eq!(
        reqs.iter()
            .filter(|request| request.starts_with("POST /projects/")
                && request.contains("memberships.json"))
            .count(),
        8
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn membership_failure_does_not_stop_other_roles_or_projects() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-apply-partial-membership");
    let mut responses = vec![projects_response(
        2,
        100,
        &[(11, "First", "first"), (22, "Second", "second")],
    )];
    persist_identities(&storage);
    responses.push(MockResponse::ok(service_roles_payload()));
    responses.push(MockResponse::status(403, "{}"));
    for _ in 0..7 {
        responses.extend(membership_added());
    }
    let (base, requests, server) = sequence(responses);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, false).unwrap();
    assert!(!report.ready);
    assert_eq!(report.projects.len(), 2);
    assert!(!report.projects[0].ready);
    assert!(report.projects[1].ready);
    assert_eq!(report.summary.added, 7);
    assert_eq!(report.summary.warnings, 1);
    assert_eq!(report.summary.errors, 1);
    assert_eq!(report.projects[0].memberships[0].status, "blocked");
    let error = report.projects[0].error.as_ref().unwrap();
    assert_eq!(error["kind"], "http");
    assert_eq!(error["status"], 403);
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 24, "{reqs:?}");
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
