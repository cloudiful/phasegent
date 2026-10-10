use super::{empty_memberships, membership_with, projects_response, service_roles_payload};
use crate::auth;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::providers::redmine::contract_tests::bootstrap_auto::{
    SERVICE_ROLES, seed_admin_only, temp_db, user_list_empty, user_list_with,
};
use crate::providers::redmine::contract_tests::support::{MockResponse, role_collection, sequence};
use std::fs;

const ALL_USERS: [(u64, &str); 4] = [
    (11, "phasegent-orchestrator"),
    (22, "phasegent-executor"),
    (33, "phasegent-reviewer"),
    (44, "phasegent-explore"),
];

fn assert_no_identity_writes(storage: &crate::infra::storage::Storage) {
    for (role, _, _) in SERVICE_ROLES {
        assert!(storage.load_credential(role, "redmine").unwrap().is_none());
        assert!(auth::load_redmine_user(role, storage).unwrap().is_none());
    }
}

#[test]
fn preview_reports_plans_without_any_write() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-preview-readonly");
    let (base, requests, server) = sequence(vec![
        projects_response(1, 100, &[(44, "Workflow", "workflow")]),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(service_roles_payload()),
        membership_with(20, 11, "phasegent-orchestrator", vec![3]),
    ]);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, true).unwrap();
    assert!(report.dry_run);
    assert!(report.ready, "existing + added are actionable in preview");
    assert_eq!(report.mode, "all-projects");
    assert_eq!(report.projects.len(), 1);
    let project = &report.projects[0];
    assert_eq!(project.project_id, 44);
    assert!(project.error.is_none());
    let statuses: Vec<&str> = project
        .memberships
        .iter()
        .map(|membership| membership.status.as_str())
        .collect();
    assert_eq!(statuses, ["existing", "added", "added", "added"]);
    assert!(report.roles.iter().all(|role| role.status == "existing"));
    assert_eq!(report.summary.existing, 1);
    assert_eq!(report.summary.added, 3);
    assert_no_identity_writes(&storage);

    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 7, "{reqs:?}");
    assert!(
        !reqs
            .iter()
            .any(|request| request.starts_with("POST") || request.starts_with("PUT")),
        "preview must never write: {reqs:?}"
    );
    assert_eq!(
        reqs.iter()
            .filter(|r| r.starts_with("GET /users.json"))
            .count(),
        4
    );
    assert_eq!(
        reqs.iter()
            .filter(|r| r.starts_with("GET /roles.json"))
            .count(),
        1
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn preview_plans_creation_for_a_missing_user() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-preview-planned");
    let (base, requests, server) = sequence(vec![
        projects_response(1, 100, &[(44, "Workflow", "workflow")]),
        MockResponse::ok(user_list_with(&ALL_USERS[..3])),
        MockResponse::ok(user_list_with(&ALL_USERS[..3])),
        MockResponse::ok(user_list_with(&ALL_USERS[..3])),
        MockResponse::ok(user_list_empty()),
        MockResponse::ok(service_roles_payload()),
        empty_memberships(),
    ]);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, true).unwrap();
    assert!(report.ready, "planned creation is actionable in preview");
    let explore = report.projects[0]
        .memberships
        .iter()
        .find(|membership| membership.role == "explore")
        .expect("explore membership planned");
    assert_eq!(explore.status, "planned");
    assert_eq!(explore.user_id, None, "no key read and no user id known");
    assert_eq!(report.summary.planned, 1);
    assert_no_identity_writes(&storage);

    let _ = requests.recv().unwrap();
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn preview_warns_on_a_missing_or_ambiguous_role() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-preview-role-warning");
    let (base, requests, server) = sequence(vec![
        projects_response(1, 100, &[(44, "Workflow", "workflow")]),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(user_list_with(&ALL_USERS)),
        MockResponse::ok(role_collection(&[(3, "Maintainer"), (4, "Developer")])),
        empty_memberships(),
    ]);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, true).unwrap();
    assert!(!report.ready, "a missing role keeps the project non-ready");
    let reporter = report.projects[0]
        .memberships
        .iter()
        .find(|membership| membership.role == "reviewer")
        .expect("reviewer membership");
    assert_eq!(reporter.status, "warning");
    assert_eq!(reporter.warning.as_deref(), Some("role was not found"));
    assert_eq!(report.summary.warnings, 2);
    assert_no_identity_writes(&storage);

    let _ = requests.recv().unwrap();
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn role_read_failure_is_reported_for_every_project() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-preview-role-read-failure");
    let (base, requests, server) = sequence(vec![
        projects_response(
            2,
            100,
            &[(44, "Workflow", "workflow"), (55, "Docs", "docs")],
        ),
        MockResponse::ok(user_list_empty()),
        MockResponse::ok(user_list_empty()),
        MockResponse::ok(user_list_empty()),
        MockResponse::ok(user_list_empty()),
        MockResponse::status(403, "{}"),
    ]);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, true).unwrap();
    assert!(!report.ready);
    assert_eq!(report.projects.len(), 2);
    assert_eq!(report.summary.errors, 2);
    assert!(report.projects.iter().all(|project| {
        project
            .error
            .as_ref()
            .is_some_and(|error| error["kind"] == "http")
            && project
                .memberships
                .iter()
                .all(|membership| membership.status == "blocked")
    }));
    assert_no_identity_writes(&storage);
    let reqs = requests.recv().unwrap();
    assert_eq!(
        reqs.len(),
        6,
        "no project membership read can resolve roles"
    );
    assert!(reqs[5].starts_with("GET /roles.json"));
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
