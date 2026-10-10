use super::{
    create_role, membership_added, mirror_env, mirror_registration, project_created, seed_admin,
    temp_db, user_list_empty,
};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, assert_request_with_key, sequence,
};
use std::fs;

#[test]
fn explore_create_failure_never_falls_back_to_another_role_key() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("create-failure");
    let (_key, _url) = mirror_env();
    super::seed_three_account_install(&storage);
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(super::closed_status()),
        MockResponse::ok(super::bootstrap_project()),
        MockResponse::ok(user_list_empty()),
        MockResponse::error(422, r#"{"errors":["Login format is invalid"]}"#),
    ]);
    seed_admin(&storage, &base);

    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("a rejected explore creation must fail the bootstrap");
    let message = error.json()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("could not create the explore user"),
        "got: {message}"
    );
    for (_, _, login) in super::SERVICE_ROLES.iter().take(3) {
        assert!(
            !message.contains(login),
            "the failure must not borrow another role identity: {message}"
        );
    }
    assert!(
        storage
            .load_credential(Role::Explore, "redmine")
            .unwrap()
            .is_none(),
        "a failed create must not cache a borrowed credential"
    );
    assert!(
        storage.load_redmine_user(Role::Explore).unwrap().is_none(),
        "a failed create must not cache an identity"
    );
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 5, "explore create failure: {reqs:?}");
    assert_request_with_key(&reqs[4], "POST", "/users.json", None, "admin-redmine-key");
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn explore_api_key_failure_is_reported_without_writing_a_membership() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("key-failure");
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(super::closed_status()),
        MockResponse::ok(super::bootstrap_project()),
        // Reuse path: every login lookup hits, the explore key read fails.
        MockResponse::ok(super::user_list_with(&super::SERVICE_LOGINS)),
        MockResponse::ok(super::user_get_with_key(
            11,
            "phasegent-orchestrator",
            "orchestrator-key",
        )),
        MockResponse::ok(super::user_list_with(&super::SERVICE_LOGINS)),
        MockResponse::ok(super::user_get_with_key(
            22,
            "phasegent-executor",
            "executor-key",
        )),
        MockResponse::ok(super::user_list_with(&super::SERVICE_LOGINS)),
        MockResponse::ok(super::user_get_with_key(
            33,
            "phasegent-reviewer",
            "reviewer-key",
        )),
        MockResponse::ok(super::user_list_with(&super::SERVICE_LOGINS)),
        MockResponse::error(500, r#"{"errors":["boom"]}"#),
    ]);
    seed_admin(&storage, &base);

    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("an unreadable explore API key must fail the bootstrap");
    let message = error.json()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("could not retrieve the explore user API key"),
        "got: {message}"
    );
    assert!(
        !message.contains("reviewer-key") && !message.contains("orchestrator-key"),
        "the error must not echo another role key: {message}"
    );
    assert!(
        storage
            .load_credential(Role::Explore, "redmine")
            .unwrap()
            .is_none(),
        "no credential may be cached without its own key read"
    );
    let reqs = requests.recv().unwrap();
    // 3 project + 4 login lookups + 3 key reads + 1 failing explore key read.
    assert_eq!(reqs.len(), 11, "explore key failure: {reqs:?}");
    assert!(
        !reqs.iter().any(|req| req.contains("memberships")),
        "no membership call may run before every identity is resolved: {reqs:?}"
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn explore_create_payload_never_carries_a_password_or_admin_flag() {
    // The explore service account is provisioned exactly like the other agent
    // roles: generated password, non-admin, active, deterministic mail.
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("create-payload");
    let (_key, _url) = mirror_env();
    super::seed_three_account_install(&storage);
    let mut responses = project_created();
    responses.extend(create_role(44, "phasegent-explore", "explore-key"));
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    responses.extend(mirror_registration());
    let (base, requests, server) = sequence(responses);
    seed_admin(&storage, &base);

    crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    let reqs = requests.recv().unwrap();
    // Explore is resolved through the paginated login scan before any create.
    assert!(reqs[3].starts_with("GET /users.json?"), "{}", reqs[3]);
    let create = reqs
        .iter()
        .find(|req| req.starts_with("POST /users.json"))
        .expect("explore create request");
    assert!(create.contains(r#""generate_password":true"#), "{create}");
    assert!(!create.contains(r#""password""#), "{create}");
    assert!(create.contains(r#""admin":false"#), "{create}");
    assert!(create.contains(r#""status":1"#), "{create}");
    assert!(
        create.contains(r#""mail":"phasegent-explore@phasegent.local""#),
        "{create}"
    );
    assert_eq!(
        storage.load_redmine_user(Role::Explore).unwrap(),
        Some((44, "phasegent-explore".to_owned()))
    );
    assert_eq!(
        storage
            .load_credential(Role::Explore, "redmine")
            .unwrap()
            .as_deref(),
        Some("explore-key")
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
