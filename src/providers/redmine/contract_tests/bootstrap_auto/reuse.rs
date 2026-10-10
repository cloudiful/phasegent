use super::{
    SERVICE_ROLES, bootstrap_project, closed_status, existing_role, membership_added,
    mirror_registration, seed_admin_only, temp_db, user_list_with,
};
use crate::auth;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{MockResponse, sequence};
use std::fs;

fn mirror_env() -> (EnvGuard, EnvGuard) {
    (
        EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key"),
        EnvGuard::set(
            "PHASEGENT_REDMINE_REPOSITORY_URL",
            "https://git.example.com/owner/repo.git",
        ),
    )
}

/// Every deterministic user already exists; provisioning must find them by
/// login and only read their keys, never POST.
fn existing_users_sequence(key_suffix: &str) -> Vec<MockResponse> {
    let existing: Vec<(u64, &str)> = SERVICE_ROLES
        .iter()
        .map(|(_, id, login)| (*id, *login))
        .collect();
    let full_list = user_list_with(&existing);
    let mut responses = vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(bootstrap_project()),
    ];
    for (role, id, login) in SERVICE_ROLES {
        responses.extend(existing_role(
            &full_list,
            id,
            login,
            &format!("{}-{key_suffix}", role.as_str()),
        ));
    }
    for _ in 0..4 {
        responses.extend(membership_added());
    }
    responses.extend(mirror_registration());
    responses
}

#[test]
fn bootstrap_reuses_existing_service_users_found_by_login() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("lookup-existing");
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(existing_users_sequence("existing-key"));
    seed_admin_only(&storage, &base);
    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(result.user_memberships.len(), 4);
    let reqs = requests.recv().unwrap();
    // 3 project + 8 provisioning (lookup + key per role) + 12 membership + 2 mirror.
    assert_eq!(reqs.len(), 25, "lookup-existing must be 25: {reqs:?}");
    assert!(
        !reqs.iter().any(|r| r.starts_with("POST /users.json")),
        "lookup-existing must never create: {reqs:?}"
    );
    assert_eq!(
        reqs.iter()
            .filter(|r| r.contains("GET /users.json?"))
            .count(),
        4,
        "one lookup per role: {reqs:?}"
    );
    for (role, _, login) in SERVICE_ROLES {
        assert_eq!(
            storage.load_credential(role, "redmine").unwrap().as_deref(),
            Some(format!("{}-existing-key", role.as_str()).as_str()),
            "cached key for {login}"
        );
    }
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn bootstrap_legacy_credential_without_user_id_looks_up_before_creating() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("legacy-lookup");
    let (_key, _url) = mirror_env();
    // Legacy rows: credentials exist but no role_redmine_user mapping.
    // The deterministic users already exist server-side, so provisioning
    // must find them and never POST.
    for (role, _, _) in SERVICE_ROLES.iter().take(3) {
        storage
            .save_credential(*role, "redmine", &format!("stale-{role}-key"))
            .unwrap();
    }
    let (base, requests, server) = sequence(existing_users_sequence("legacy-key"));
    seed_admin_only(&storage, &base);
    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(result.user_memberships.len(), 4);
    let reqs = requests.recv().unwrap();
    assert!(
        !reqs.iter().any(|r| r.starts_with("POST /users.json")),
        "legacy lookup must not create duplicates: {reqs:?}"
    );
    // Stale keys overwritten with admin-read keys.
    for (role, id, login) in SERVICE_ROLES {
        assert_eq!(
            storage.load_credential(role, "redmine").unwrap().as_deref(),
            Some(format!("{}-legacy-key", role.as_str()).as_str()),
            "admin-read key must replace the stale {login} credential"
        );
        assert_eq!(
            storage.load_redmine_user(role).unwrap(),
            Some((id, login.to_owned())),
            "identity row for {login}"
        );
    }
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn legacy_lookup_still_refreshes_a_legacy_close_status_row() {
    // The legacy arm keeps its own admin endpoint resolution: seeding the
    // admin config directly (as `admin auth setup` does) must still drive a
    // full lookup-based bootstrap.
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("legacy-admin-config");
    let (_key, _url) = mirror_env();
    let (base, requests, server) = sequence(existing_users_sequence("legacy-admin-key"));
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    storage
        .save_redmine_config(
            Role::Admin,
            &auth::RedmineStoredConfig {
                api_base: Some(base.clone()),
                project_id: None,
                close_status_id: None,
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert!(result.ready(), "all four memberships must be actionable");
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 25, "legacy admin config run: {reqs:?}");
    let persisted = auth::load_redmine_config(Role::Explore, &storage)
        .unwrap()
        .expect("explore role settings must be persisted");
    assert_eq!(persisted.close_status_id, Some(5));
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
