use super::{projects_response, service_roles_payload};
use crate::auth;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::providers::redmine::contract_tests::bootstrap_auto::{
    SERVICE_ROLES, seed_admin_only, temp_db, user_list_with,
};
use crate::providers::redmine::contract_tests::support::{
    MockResponse, membership_collection_page, sequence,
};
use std::fs;

#[test]
fn shared_service_identity_is_rejected_before_membership_preview() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-identity-collision");
    // Every deterministic login resolves to the same Redmine user id.
    let shared = user_list_with(&[
        (11, "phasegent-orchestrator"),
        (11, "phasegent-executor"),
        (11, "phasegent-reviewer"),
        (11, "phasegent-explore"),
    ]);
    let (base, requests, server) = sequence(vec![
        projects_response(1, 100, &[(44, "Workflow", "workflow")]),
        MockResponse::ok(shared.clone()),
        MockResponse::ok(shared),
    ]);
    seed_admin_only(&storage, &base);

    let error = crate::workflow::bootstrap_all_projects(None, true).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(
        error.json()["message"]
            .as_str()
            .unwrap()
            .contains("distinct"),
        "{}",
        error.json()
    );
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 3, "collision stops the third lookup: {reqs:?}");
    assert!(!reqs.iter().any(|request| request.starts_with("POST")));
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn preview_uses_persisted_identity_authority_without_login_lookups() {
    let _lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("global-preview-persisted-identities");
    let stored_logins = [
        "saved-orchestrator",
        "saved-executor",
        "saved-reviewer",
        "saved-explore",
    ];
    for ((role, id, _), login) in SERVICE_ROLES.iter().zip(stored_logins) {
        storage
            .save_credential(*role, "redmine", &format!("{}-key", role.as_str()))
            .unwrap();
        auth::save_redmine_user(*role, *id, login, &storage).unwrap();
    }
    let mut responses = vec![
        projects_response(1, 100, &[(44, "Workflow", "workflow")]),
        MockResponse::ok(service_roles_payload()),
    ];
    for (index, (role, id, _)) in SERVICE_ROLES.iter().enumerate() {
        let role_id = match role.as_str() {
            "orchestrator" => 3,
            "executor" => 4,
            _ => 5,
        };
        responses.push(MockResponse::ok(membership_collection_page(
            4,
            1,
            Some((index as u64 + 101, *id, "stored-user", vec![role_id])),
        )));
    }
    let (base, requests, server) = sequence(responses);
    seed_admin_only(&storage, &base);

    let report = crate::workflow::bootstrap_all_projects(None, true).unwrap();
    assert!(report.ready);
    assert_eq!(
        report
            .roles
            .iter()
            .map(|role| role.login.as_str())
            .collect::<Vec<_>>(),
        stored_logins
    );
    assert!(
        report.projects[0]
            .memberships
            .iter()
            .all(|membership| membership.status == "existing")
    );
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 6, "{reqs:?}");
    assert!(
        !reqs
            .iter()
            .any(|request| request.starts_with("GET /users.json")),
        "persisted identities are authoritative in preview: {reqs:?}"
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
