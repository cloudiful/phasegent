use super::{
    SERVICE_ROLES, bootstrap_project, closed_status, create_role, membership_added,
    mirror_registration, seed_admin_only, temp_db,
};
use crate::auth;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{MockResponse, sequence};
use std::fs;

#[test]
fn bootstrap_fails_without_admin_credential_without_falling_back() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("missing-admin");
    // Only a non-admin role key is present; bootstrap must not fall back.
    storage
        .save_credential(Role::Orchestrator, "redmine", "orchestrator-only-key")
        .unwrap();
    storage
        .save_redmine_config(
            Role::Admin,
            &auth::RedmineStoredConfig {
                api_base: Some("http://127.0.0.1:9".to_owned()),
                project_id: None,
                close_status_id: None,
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("bootstrap without admin key must fail");
    let json = error.json();
    assert_eq!(json["kind"], "auth");
    let message = json["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("could not read Redmine API key"),
        "got: {message}"
    );
    assert!(
        !message.contains("orchestrator-only-key"),
        "error must not leak role key: {message}"
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn bootstrap_never_provisions_the_retired_tester_identity() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("no-retired-role");
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    let mut responses = vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(bootstrap_project()),
    ];
    for (role, id, login) in SERVICE_ROLES {
        responses.extend(create_role(id, login, &format!("{}-key", role.as_str())));
    }
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    responses.extend(mirror_registration());
    let (base, requests, server) = sequence(responses);
    seed_admin_only(&storage, &base);
    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(
        result.user_memberships.len(),
        4,
        "bootstrap must provision exactly the four service roles"
    );
    assert!(
        !result
            .user_memberships
            .iter()
            .any(|m| m.user_login.contains("tester")),
        "retired role membership present: {:?}",
        result.user_memberships
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 29, "expected 29 bootstrap requests: {reqs:?}");
    assert!(
        !reqs.iter().any(|r| r.contains("tester")),
        "bootstrap must not touch the retired identity: {reqs:?}"
    );
}
