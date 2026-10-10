use super::{
    SERVICE_ROLES, bootstrap_project, closed_status, create_role, membership_added,
    mirror_registration, seed_admin_only, temp_db,
};
use crate::auth;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, assert_request_with_key, issue_collection, issue_response, sequence, strings,
};
use std::fs;

#[test]
fn issue_create_automatically_bootstraps_once_before_returning_issue() {
    let _environment_lock = lock_workflow_tests();
    crate::workflow::clear_completed_bootstraps_for_tests();
    let (directory, _guard, storage) = temp_db("auto");
    let _mirror_key = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
        "mirror-bearer-key",
    );
    let _mirror_url = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );

    // Admin-only provisioning: only the admin credential is seeded. Missing
    // role credentials no longer block provisioning; the admin API
    // provisions every deterministic service user.
    let (base, requests, server) = sequence(bootstrap_sequence());
    // Seed only admin: proves admin credential is sufficient.
    seed_admin_only(&storage, &base);
    // Also seed orchestrator config row so the CLI resolver has a base
    // before bootstrap persists it (mirrors production where the operator
    // may have run `auth setup` for orchestrator previously). The
    // credential itself is intentionally absent.
    storage
        .save_redmine_config(
            Role::Orchestrator,
            &auth::RedmineStoredConfig {
                api_base: Some(base.clone()),
                project_id: Some("999".to_owned()),
                close_status_id: Some(5),
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();

    assert_eq!(
        crate::cli::run_with_role(issue_create_args(&base, "Created"), Some("orchestrator")),
        0
    );
    assert_eq!(
        crate::cli::run_with_role(
            issue_create_args(&base, "Created again"),
            Some("orchestrator")
        ),
        0
    );
    assert_eq!(
        crate::cli::run_with_role(
            strings([
                "--provider",
                "redmine",
                "--api-base",
                &base,
                "--repository",
                "owner/repo",
                "issue",
                "search",
                "--all",
                "--state",
                "all",
            ]),
            Some("orchestrator")
        ),
        0
    );
    assert_eq!(
        crate::cli::run_with_role(
            strings([
                "--provider",
                "redmine",
                "--api-base",
                &base,
                "--repository",
                "owner/repo",
                "--project-id",
                "99",
                "issue",
                "create",
                "--title",
                "Explicit",
                "--body",
                "Body",
            ]),
            Some("orchestrator")
        ),
        0
    );

    assert_bootstrap_requests(&requests.recv().unwrap());
    assert_provisioned_identities(&storage);
    let stored = auth::load_redmine_config(Role::Orchestrator, &storage)
        .unwrap()
        .unwrap();
    assert_eq!(stored.project_id, None);
    assert_eq!(stored.close_status_id, Some(5));
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

fn issue_create_args(base: &str, title: &str) -> Vec<String> {
    strings([
        "--provider",
        "redmine",
        "--api-base",
        base,
        "--repository",
        "owner/repo",
        "issue",
        "create",
        "--title",
        title,
        "--body",
        "Body",
    ])
}

/// Project bootstrap, four-role provisioning, four memberships, mirror
/// registration, then the issue traffic of this test.
fn bootstrap_sequence() -> Vec<MockResponse> {
    let mut responses = vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(bootstrap_project()),
    ];
    for (role, id, login) in SERVICE_ROLES {
        responses.extend(create_role(
            id,
            login,
            &format!("{}-provisioned-key", role.as_str()),
        ));
    }
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    responses.extend(mirror_registration());
    responses.extend([
        // First issue create (orchestrator key provisioned above).
        MockResponse::ok(issue_response(80, "Created", "Body", false, &[])),
        // Second issue create (bootstrap result reused via cache).
        MockResponse::ok(issue_response(81, "Created again", "Body", false, &[])),
        // Issue search.
        MockResponse::ok(issue_collection(1, 100, &[(80, "Created", false)])),
        // Explicit project id bypasses bootstrap.
        MockResponse::ok(issue_response(82, "Explicit", "Body", false, &[])),
    ]);
    responses
}

/// Every provisioned role keeps its own admin-read API key and identity row.
fn assert_provisioned_identities(storage: &crate::infra::storage::Storage) {
    for (role, id, login) in SERVICE_ROLES {
        assert_eq!(
            storage.load_credential(role, "redmine").unwrap().as_deref(),
            Some(format!("{}-provisioned-key", role.as_str()).as_str()),
            "provisioned key for {role} must be cached"
        );
        assert_eq!(
            storage.load_redmine_user(role).unwrap(),
            Some((id, login.to_owned())),
            "provisioned identity for {role} must be cached"
        );
    }
}

fn assert_bootstrap_requests(requests: &[String]) {
    // 3 project + 12 provisioning + 12 membership + 2 mirror + 4 issue = 33.
    assert_eq!(requests.len(), 33, "unexpected request count: {requests:?}");
    assert_request_with_key(
        &requests[0],
        "GET",
        "/projects/owner-repo.json",
        None,
        "admin-redmine-key",
    );
    // No legacy per-role current-user lookups remain.
    assert!(
        !requests.iter().any(|r| r.contains("/users/current.json")),
        "admin-only provisioning must not call /users/current.json: {requests:?}"
    );
    // Provisioning lookups use the admin key on /users.json.
    assert_request_with_key(
        &requests[3],
        "GET",
        "/users.json?",
        None,
        "admin-redmine-key",
    );
    // Service creates request generated passwords, never a password field.
    assert!(requests[4].starts_with("POST /users.json"));
    assert!(
        requests[4].contains(r#""generate_password":true"#),
        "create must use generate_password: {}",
        requests[4]
    );
    assert!(
        !requests[4].contains(r#""password""#),
        "create must not send password: {}",
        requests[4]
    );
    assert!(
        requests[4].contains(r#""login":"phasegent-orchestrator""#),
        "{}",
        requests[4]
    );
    // Memberships use deterministic ids and the documented role mapping.
    assert!(requests[17].contains(r#""user_id":11,"role_ids":[3]"#));
    assert!(requests[20].contains(r#""user_id":22,"role_ids":[4]"#));
    assert!(requests[23].contains(r#""user_id":33,"role_ids":[5]"#));
    assert!(requests[26].contains(r#""user_id":44,"role_ids":[5]"#));
    assert!(
        !requests.iter().any(|r| r.contains("phasegent-tester")),
        "bootstrap must not provision the retired role: {requests:?}"
    );
    assert!(requests[32].contains(r#""project_id":99"#));
}
