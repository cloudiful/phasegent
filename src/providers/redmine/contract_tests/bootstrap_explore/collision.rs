use super::{
    SERVICE_ROLES, project_created, seed_admin, seed_persisted, seed_three_account_install, temp_db,
};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::sequence;
use std::fs;

#[test]
fn explore_collision_aborts_before_membership_and_setting_writes() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("collision");
    // A three-account install whose explore row points at the orchestrator
    // identity: the guard must reject the pairing, naming explore.
    seed_three_account_install(&storage);
    seed_persisted(
        &storage,
        Role::Explore,
        11,
        "phasegent-orchestrator",
        "explore-key",
    );
    let (base, requests, server) = sequence(project_created());
    seed_admin(&storage, &base);

    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("bootstrap must fail when explore collides with the orchestrator");
    assert_eq!(error.json()["kind"], "config");
    let message = error.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("distinct users"), "got: {message}");
    assert!(
        message.contains("explore=phasegent-orchestrator"),
        "the collision must name explore: {message}"
    );
    assert!(
        message.contains("orchestrator=phasegent-orchestrator"),
        "the collision must name the other role: {message}"
    );

    let reqs = requests.recv().unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "every identity was persisted, so the guard fires without HTTP: {reqs:?}"
    );
    for req in reqs.iter() {
        assert!(
            !req.contains("memberships"),
            "no membership call on a distinct-user failure: {req}"
        );
    }

    // Role project/status settings stay untouched for every role.
    for (role, _, login) in SERVICE_ROLES {
        let stored = crate::auth::load_redmine_config(role, &storage)
            .unwrap()
            .unwrap_or_default();
        assert_eq!(
            stored.close_status_id, None,
            "{login} settings must not be persisted"
        );
    }
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
