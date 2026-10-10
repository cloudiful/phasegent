use super::{projects_response, service_roles_payload};
use crate::auth;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::bootstrap_auto::{
    SERVICE_ROLES, create_role, membership_added, seed_admin_only, temp_db,
};
use crate::providers::redmine::contract_tests::support::{
    MockResponse, membership_collection, sequence,
};
use std::fs;

fn provision_responses() -> Vec<MockResponse> {
    let mut responses = Vec::new();
    for (role, id, login) in SERVICE_ROLES {
        responses.extend(create_role(id, login, &format!("{}-key", role.as_str())));
    }
    responses
}

#[test]
fn apply_provisions_once_and_adds_memberships_without_settings_or_mirrors() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-apply-fresh");
    let mut responses = vec![projects_response(1, 100, &[(44, "Workflow", "workflow")])];
    responses.extend(provision_responses());
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    let (base, requests, server) = sequence(responses);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, false).unwrap();
    assert!(!report.dry_run);
    assert!(report.ready);
    assert!(
        report.roles.iter().all(|role| role.status == "provisioned"),
        "{:?}",
        report.roles
    );
    assert!(
        report.projects[0]
            .memberships
            .iter()
            .all(|membership| membership.status == "added")
    );
    assert_eq!(report.summary.added, 4);

    let reqs = requests.recv().unwrap();
    assert!(
        !reqs
            .iter()
            .any(|request| request.starts_with("POST /projects.json")
                || request.contains("/sys/redmine_git_mirror")
                || request.contains("issue_statuses")),
        "global apply must not create projects, mirrors, or touch statuses: {reqs:?}"
    );
    assert_eq!(
        reqs.iter()
            .filter(|request| request.starts_with("POST /users.json"))
            .count(),
        4
    );
    assert!(
        reqs.iter()
            .any(|request| request.starts_with("POST /projects/44/memberships.json"))
    );
    // Identities and keys are persisted; no project settings/cache is written.
    for (role, _, _) in SERVICE_ROLES {
        assert!(storage.load_credential(role, "redmine").unwrap().is_some());
        assert!(storage.load_redmine_user(role).unwrap().is_some());
    }
    assert!(
        auth::load_redmine_config(Role::Explore, &storage)
            .unwrap()
            .is_none()
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn apply_preserves_existing_membership_roles() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-apply-additive");
    let mut responses = vec![projects_response(1, 100, &[(44, "Workflow", "workflow")])];
    responses.extend(provision_responses());
    // Orchestrator: fresh add.
    responses.extend(membership_added());
    // Executor already holds Maintainer (3); ensure appends Developer (4).
    responses.push(MockResponse::ok(service_roles_payload()));
    responses.push(MockResponse::ok(membership_collection(Some((
        55,
        22,
        "phasegent-executor",
        vec![3],
    )))));
    responses.push(MockResponse::ok("{}"));
    // Reviewer + explore: fresh add.
    responses.extend(membership_added());
    responses.extend(membership_added());
    let (base, requests, server) = sequence(responses);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, false).unwrap();
    assert!(report.ready);
    let executor = report.projects[0]
        .memberships
        .iter()
        .find(|membership| membership.role == "executor")
        .unwrap();
    assert_eq!(executor.status, "updated");
    let reqs = requests.recv().unwrap();
    let update = reqs
        .iter()
        .find(|request| request.starts_with("PUT /memberships/55.json"))
        .expect("additive update");
    assert!(update.contains(r#""role_ids":[3,4]"#), "{update}");
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn apply_reruns_idempotently_without_user_or_membership_writes() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-apply-rerun");
    // Persisted identities: provisioning makes no HTTP call at all.
    for (role, id, login) in SERVICE_ROLES {
        storage
            .save_credential(role, "redmine", &format!("{}-key", role.as_str()))
            .unwrap();
        auth::save_redmine_user(role, id, login, &storage).unwrap();
    }
    let mut responses = vec![projects_response(1, 100, &[(44, "Workflow", "workflow")])];
    for (id, login, role_id) in [
        (11, "phasegent-orchestrator", 3),
        (22, "phasegent-executor", 4),
        (33, "phasegent-reviewer", 5),
        (44, "phasegent-explore", 5),
    ] {
        responses.push(MockResponse::ok(service_roles_payload()));
        responses.push(MockResponse::ok(membership_collection(Some((
            id,
            id,
            login,
            vec![role_id],
        )))));
    }
    let (base, requests, server) = sequence(responses);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, false).unwrap();
    assert!(report.ready);
    assert!(
        report.projects[0]
            .memberships
            .iter()
            .all(|membership| membership.status == "existing")
    );
    let reqs = requests.recv().unwrap();
    assert!(
        !reqs
            .iter()
            .any(|request| request.starts_with("POST") || request.starts_with("PUT")),
        "rerun must not create users or write memberships: {reqs:?}"
    );
    assert!(
        !reqs
            .iter()
            .any(|request| request.starts_with("GET /users.json")),
        "persisted identities need no lookup: {reqs:?}"
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
