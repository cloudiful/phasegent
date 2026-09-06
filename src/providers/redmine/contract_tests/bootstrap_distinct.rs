#![allow(unused_imports)]
use super::support;
use super::support::{
    MockResponse, TEST_API_KEY, current_user_response, git_mirror_response, issue_collection,
    issue_response, membership_collection, membership_collection_page, mirror_env, one,
    project_collection, project_response, provider, role_collection, role_collection_page,
    sequence, strings, time_entry_activities, time_entry_collection, time_entry_response,
    user_from_response, version_collection, version_collection_page,
};
use crate::auth;
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use std::{fs, time};

fn temp_db(label: &str) -> (std::path::PathBuf, EnvGuard, Storage) {
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-distinct-{label}-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = directory.join(crate::infra::storage::DB_FILENAME);
    let guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let storage = Storage::open_at(&db_path).unwrap();
    (directory, guard, storage)
}

fn seed_admin(storage: &Storage, base: &str) {
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    storage
        .save_redmine_config(
            Role::Admin,
            &auth::RedmineStoredConfig {
                api_base: Some(base.to_owned()),
                project_id: None,
                close_status_id: None,
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
}

fn seed_persisted(storage: &Storage, role: Role, id: u64, login: &str, key: &str) {
    storage.save_redmine_user(role, id, login).unwrap();
    storage.save_credential(role, "redmine", key).unwrap();
}

#[test]
fn bootstrap_fails_with_distinct_users_error_when_two_keys_resolve_to_same_user() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("users");
    // Phase 2: distinct guard runs on provisioned identities. Seed
    // persisted mappings where orchestrator and executor share id 11 so
    // provisioning reuses without HTTP and the guard fires.
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
            })
            .to_string(),
        ),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow issues for owner/repo",
        )),
    ]);
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
    seed_persisted(&storage, Role::Tester, 44, "phasegent-tester", "tester-key");

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
    for (index, request) in observed_requests.iter().enumerate() {
        assert!(
            !request.starts_with("POST /projects/44/memberships.json"),
            "no membership POST should fire on distinct-user failure (index {index}): {request}"
        );
        assert!(
            !request.starts_with("PUT /memberships/"),
            "no membership PUT should fire on distinct-user failure (index {index}): {request}"
        );
        assert!(
            !request.contains("/users.json") && !request.contains("/users/"),
            "no user API should fire when persisted users are reused (index {index}): {request}"
        );
    }

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
fn bootstrap_fails_when_tester_collides_with_existing_user() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("tester-distinct");
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({
                "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
            })
            .to_string(),
        ),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow issues for owner/repo",
        )),
    ]);
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
    seed_persisted(
        &storage,
        Role::Reviewer,
        33,
        "phasegent-reviewer",
        "reviewer-key",
    );
    // Tester reuses executor's id.
    seed_persisted(
        &storage,
        Role::Tester,
        22,
        "phasegent-executor",
        "tester-key",
    );

    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("bootstrap must fail when tester collides with executor");
    let message = error.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("distinct users"), "got: {message}");
    assert!(
        message.contains("tester"),
        "tester collision must be mentioned: {message}"
    );
    let reqs = requests.recv().unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "must stop after project bootstrap on tester collision: {reqs:?}"
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

#[test]
fn bootstrap_succeeds_with_distinct_tester_when_configured() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("tester-ok");
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    // Distinct persisted users: no user HTTP, only membership + mirror.
    let (base, _requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(
            serde_json::json!({"issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]})
                .to_string(),
        ),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow",
        )),
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        MockResponse::error(404, r#"{"errors":["mirror not found"]}"#),
        MockResponse::status(
            202,
            support::git_mirror_response(
                901,
                44,
                "mirror_44_owner_repo",
                "pending",
                Some("https://git.example.com/owner/repo.git"),
                Some("/var/redmine/repos/owner_repo.git"),
                None,
            ),
        ),
    ]);
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
    seed_persisted(
        &storage,
        Role::Reviewer,
        33,
        "phasegent-reviewer",
        "reviewer-key",
    );
    seed_persisted(&storage, Role::Tester, 44, "phasegent-tester", "tester-key");
    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(
        result.user_memberships.len(),
        4,
        "with tester configured must have 4 memberships"
    );
    assert!(
        result
            .user_memberships
            .iter()
            .any(|m| m.user_id == 44 && m.role_name == "Reporter")
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
