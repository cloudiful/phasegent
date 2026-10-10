use super::{
    assert_no_membership_or_user_calls, project_bootstrap_responses, seed_admin, seed_persisted,
    temp_db,
};
use crate::auth;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, project_response, sequence,
};
use std::fs;

fn bootstrap_project() -> MockResponse {
    MockResponse::ok(project_response(
        44,
        "owner/repo",
        "owner-repo",
        "Workflow issues for owner/repo",
    ))
}

#[test]
fn bootstrap_fails_with_distinct_users_error_when_two_keys_resolve_to_same_user() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("users");
    // The distinct guard runs on provisioned identities. Seed persisted
    // mappings where orchestrator and executor share id 11 so provisioning
    // reuses without HTTP and the guard fires.
    let (base, requests, server) = sequence(project_bootstrap_responses(bootstrap_project()));
    // Seed after base is known so admin config points at the mock.
    seed_admin(&storage, &base);
    seed_persisted(
        &storage,
        Role::Orchestrator,
        11,
        "phasegent-orchestrator",
        "orchestrator-key",
    );
    seed_persisted(
        &storage,
        Role::Executor,
        11,
        "phasegent-orchestrator",
        "executor-key",
    );
    seed_persisted(
        &storage,
        Role::Reviewer,
        33,
        "phasegent-reviewer",
        "reviewer-key",
    );

    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("bootstrap must fail when two provisioned users share an id");
    let json = error.json();
    assert_eq!(json["kind"], "config");
    let message = json["message"]
        .as_str()
        .expect("error message missing")
        .to_owned();
    assert!(
        message.contains("distinct users"),
        "error message must mention distinct users, got: {message}"
    );
    assert!(
        message.contains("phasegent-orchestrator") || message.contains("#11"),
        "error message must describe the colliding identity, got: {message}"
    );

    // Bootstrap must abort after project bootstrap + persisted reuse and
    // never issue a membership POST/PUT or a user lookup/create.
    let observed_requests = requests.recv().unwrap();
    assert_eq!(
        observed_requests.len(),
        3,
        "bootstrap must stop after project bootstrap on distinct-user failure: {observed_requests:?}"
    );
    assert_no_membership_or_user_calls(&observed_requests);

    let stored = auth::load_redmine_config(Role::Admin, &storage)
        .expect("admin config must load")
        .expect("admin config row must still exist");
    assert_eq!(stored.project_id, None);
    assert_eq!(stored.close_status_id, None);
    assert_eq!(stored.api_base.as_deref(), Some(base.as_str()));

    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn bootstrap_fails_when_reviewer_collides_with_existing_user() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("reviewer-distinct");
    let (base, requests, server) = sequence(project_bootstrap_responses(bootstrap_project()));
    seed_admin(&storage, &base);
    seed_persisted(
        &storage,
        Role::Orchestrator,
        11,
        "phasegent-orchestrator",
        "orchestrator-key",
    );
    seed_persisted(
        &storage,
        Role::Executor,
        22,
        "phasegent-executor",
        "executor-key",
    );
    // Reviewer reuses executor's id.
    seed_persisted(
        &storage,
        Role::Reviewer,
        22,
        "phasegent-executor",
        "reviewer-key",
    );

    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("bootstrap must fail when reviewer collides with executor");
    let message = error.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("distinct users"), "got: {message}");
    assert!(
        message.contains("reviewer=phasegent-executor"),
        "reviewer collision must be named: {message}"
    );
    let reqs = requests.recv().unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "must stop after project bootstrap on reviewer collision: {reqs:?}"
    );
    for req in reqs.iter() {
        assert!(
            !req.starts_with("POST /projects/44/memberships.json"),
            "no membership on distinct failure: {req}"
        );
    }
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
