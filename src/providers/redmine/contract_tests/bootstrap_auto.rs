#![allow(unused_imports)]
use super::support;
use super::support::*;
use crate::auth;
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use std::{fs, time};

fn user_list_empty() -> String {
    serde_json::json!({
        "users": [],
        "total_count": 0,
        "limit": 100,
    })
    .to_string()
}

fn user_list_with(users: &[(u64, &str)]) -> String {
    serde_json::json!({
        "users": users.iter().map(|(id, login)| serde_json::json!({
            "id": id,
            "login": login,
            "firstname": "Phasegent",
            "lastname": login,
            "mail": format!("{login}@phasegent.local"),
        })).collect::<Vec<_>>(),
        "total_count": users.len(),
        "limit": 100,
    })
    .to_string()
}

fn user_create_response(id: u64, login: &str) -> String {
    serde_json::json!({
        "user": {
            "id": id,
            "login": login,
            "firstname": "Phasegent",
            "lastname": login,
            "mail": format!("{login}@phasegent.local"),
        }
    })
    .to_string()
}

fn user_get_with_key(id: u64, login: &str, api_key: &str) -> String {
    serde_json::json!({
        "user": {
            "id": id,
            "login": login,
            "firstname": "Phasegent",
            "lastname": login,
            "mail": format!("{login}@phasegent.local"),
            "api_key": api_key,
        }
    })
    .to_string()
}

fn closed_status() -> String {
    serde_json::json!({
        "issue_statuses": [{"id": 5, "name": "Closed", "is_closed": true}]
    })
    .to_string()
}

fn temp_db(label: &str) -> (std::path::PathBuf, EnvGuard, Storage) {
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-{label}-{}-{}",
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

fn seed_admin_only(storage: &Storage, base: &str) {
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

#[test]
fn issue_create_automatically_bootstraps_once_before_returning_issue() {
    let _environment_lock = lock_workflow_tests();
    crate::workflow::clear_completed_bootstraps_for_tests();
    let (directory, _guard, storage) = temp_db("auto");
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );

    // Phase 2 admin-only: only the admin credential is seeded. Missing
    // role credentials no longer block provisioning; the admin API
    // provisions all four deterministic service users.
    let (base, requests, server) = sequence(vec![
        // Project bootstrap (admin).
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow issues for owner/repo",
        )),
        // Orchestrator: lookup miss, create, key read.
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(11, "phasegent-orchestrator")),
        MockResponse::ok(user_get_with_key(
            11,
            "phasegent-orchestrator",
            "orchestrator-provisioned-key",
        )),
        // Executor.
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(22, "phasegent-executor")),
        MockResponse::ok(user_get_with_key(
            22,
            "phasegent-executor",
            "executor-provisioned-key",
        )),
        // Reviewer.
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(33, "phasegent-reviewer")),
        MockResponse::ok(user_get_with_key(
            33,
            "phasegent-reviewer",
            "reviewer-provisioned-key",
        )),
        // Tester (always provisioned in Phase 2).
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(44, "phasegent-tester")),
        MockResponse::ok(user_get_with_key(
            44,
            "phasegent-tester",
            "tester-provisioned-key",
        )),
        // Memberships for all four (admin).
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
        // Mirror.
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
        // First issue create (orchestrator key provisioned above).
        MockResponse::ok(support::issue_response(80, "Created", "Body", false, &[])),
        // Second issue create (bootstrap result reused via cache).
        MockResponse::ok(support::issue_response(
            81,
            "Created again",
            "Body",
            false,
            &[],
        )),
        // Issue search.
        MockResponse::ok(support::issue_collection(1, 100, &[(80, "Created", false)])),
        // Explicit project id bypasses bootstrap.
        MockResponse::ok(support::issue_response(82, "Explicit", "Body", false, &[])),
    ]);
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

    let args = strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "--api-base",
        &base,
        "--repository",
        "owner/repo",
        "issue",
        "create",
        "--title",
        "Created",
        "--body",
        "Body",
    ]);
    assert_eq!(crate::cli::run(args), 0);
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
            "--provider",
            "redmine",
            "--api-base",
            &base,
            "--repository",
            "owner/repo",
            "issue",
            "create",
            "--title",
            "Created again",
            "--body",
            "Body",
        ])),
        0
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
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
        ])),
        0
    );
    assert_eq!(
        crate::cli::run(strings([
            "--role",
            "orchestrator",
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
        ])),
        0
    );

    let requests = requests.recv().unwrap();
    // 3 project + 12 provisioning + 12 membership + 2 mirror + 4 issue = 33.
    assert_eq!(requests.len(), 33, "unexpected request count: {requests:?}");
    support::assert_request_with_key(
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
    support::assert_request_with_key(
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
    // Memberships use deterministic ids.
    assert!(requests[17].contains(r#""user_id":11,"role_ids":[3]"#));
    assert!(requests[20].contains(r#""user_id":22,"role_ids":[4]"#));
    assert!(requests[23].contains(r#""user_id":33,"role_ids":[5]"#));
    assert!(requests[26].contains(r#""user_id":44,"role_ids":[5]"#));
    // Provisioned keys persisted for downstream providers.
    assert_eq!(
        storage
            .load_credential(Role::Orchestrator, "redmine")
            .unwrap()
            .as_deref(),
        Some("orchestrator-provisioned-key")
    );
    assert_eq!(
        storage
            .load_credential(Role::Tester, "redmine")
            .unwrap()
            .as_deref(),
        Some("tester-provisioned-key")
    );
    assert_eq!(
        storage.load_redmine_user(Role::Orchestrator).unwrap(),
        Some((11, "phasegent-orchestrator".to_owned()))
    );
    assert_eq!(
        storage.load_redmine_user(Role::Tester).unwrap(),
        Some((44, "phasegent-tester".to_owned()))
    );
    assert!(requests[32].contains(r#""project_id":99"#));
    let stored = auth::load_redmine_config(Role::Orchestrator, &storage)
        .unwrap()
        .unwrap();
    assert_eq!(stored.project_id, None);
    assert_eq!(stored.close_status_id, Some(5));
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn bootstrap_reuses_existing_service_users_found_by_login() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("lookup-existing");
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    // All four deterministic users already exist; provisioning must find
    // them by login and only read their keys, never POST.
    let existing = vec![
        (11, "phasegent-orchestrator"),
        (22, "phasegent-executor"),
        (33, "phasegent-reviewer"),
        (44, "phasegent-tester"),
    ];
    let full_list = user_list_with(&existing);
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow",
        )),
        MockResponse::ok(full_list.clone()),
        MockResponse::ok(user_get_with_key(
            11,
            "phasegent-orchestrator",
            "orchestrator-existing-key",
        )),
        MockResponse::ok(full_list.clone()),
        MockResponse::ok(user_get_with_key(
            22,
            "phasegent-executor",
            "executor-existing-key",
        )),
        MockResponse::ok(full_list.clone()),
        MockResponse::ok(user_get_with_key(
            33,
            "phasegent-reviewer",
            "reviewer-existing-key",
        )),
        MockResponse::ok(full_list.clone()),
        MockResponse::ok(user_get_with_key(
            44,
            "phasegent-tester",
            "tester-existing-key",
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
    seed_admin_only(&storage, &base);
    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(result.user_memberships.len(), 4);
    let reqs = requests.recv().unwrap();
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
    assert_eq!(
        storage
            .load_credential(Role::Orchestrator, "redmine")
            .unwrap()
            .as_deref(),
        Some("orchestrator-existing-key")
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn bootstrap_rerun_reuses_persisted_users_without_user_api_calls() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("rerun-reuse");
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    let (base, requests, server) = sequence(vec![
        // First run: empty lookups, creates, key reads.
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow",
        )),
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(11, "phasegent-orchestrator")),
        MockResponse::ok(user_get_with_key(
            11,
            "phasegent-orchestrator",
            "orchestrator-rerun-key",
        )),
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(22, "phasegent-executor")),
        MockResponse::ok(user_get_with_key(
            22,
            "phasegent-executor",
            "executor-rerun-key",
        )),
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(33, "phasegent-reviewer")),
        MockResponse::ok(user_get_with_key(
            33,
            "phasegent-reviewer",
            "reviewer-rerun-key",
        )),
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(44, "phasegent-tester")),
        MockResponse::ok(user_get_with_key(
            44,
            "phasegent-tester",
            "tester-rerun-key",
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
        // Second run: project exists, no user lookup/create/key reads.
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow",
        )),
        MockResponse::ok(closed_status()),
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(Some((
            55,
            11,
            "phasegent-orchestrator",
            vec![3],
        )))),
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(Some((
            56,
            22,
            "phasegent-executor",
            vec![4],
        )))),
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(Some((
            57,
            33,
            "phasegent-reviewer",
            vec![5],
        )))),
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(Some((
            58,
            44,
            "phasegent-tester",
            vec![5],
        )))),
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
    seed_admin_only(&storage, &base);
    let first =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(first.user_memberships.len(), 4);
    let second =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(second.user_memberships.len(), 4);
    assert!(
        second
            .user_memberships
            .iter()
            .all(|m| m.status == "existing"),
        "second run must observe existing memberships: {:?}",
        second.user_memberships
    );
    let reqs = requests.recv().unwrap();
    // First run 29 + second run 12 = 41.
    assert_eq!(reqs.len(), 41, "rerun must be 41: {reqs:?}");
    let second_run = &reqs[29..];
    assert!(
        !second_run
            .iter()
            .any(|r| r.contains("/users.json") || r.contains("/users/")),
        "rerun must not touch user APIs: {second_run:?}"
    );
    // Persisted keys reused verbatim.
    assert_eq!(
        storage
            .load_credential(Role::Orchestrator, "redmine")
            .unwrap()
            .as_deref(),
        Some("orchestrator-rerun-key")
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn bootstrap_legacy_credential_without_user_id_looks_up_before_creating() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("legacy-lookup");
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    // Legacy rows: credentials exist but no role_redmine_user mapping.
    // The deterministic users already exist server-side, so provisioning
    // must find them and never POST.
    storage
        .save_credential(Role::Orchestrator, "redmine", "stale-orchestrator-key")
        .unwrap();
    storage
        .save_credential(Role::Executor, "redmine", "stale-executor-key")
        .unwrap();
    storage
        .save_credential(Role::Reviewer, "redmine", "stale-reviewer-key")
        .unwrap();
    storage
        .save_credential(Role::Tester, "redmine", "stale-tester-key")
        .unwrap();
    let existing = vec![
        (11, "phasegent-orchestrator"),
        (22, "phasegent-executor"),
        (33, "phasegent-reviewer"),
        (44, "phasegent-tester"),
    ];
    let full_list = user_list_with(&existing);
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow",
        )),
        MockResponse::ok(full_list.clone()),
        MockResponse::ok(user_get_with_key(
            11,
            "phasegent-orchestrator",
            "orchestrator-legacy-key",
        )),
        MockResponse::ok(full_list.clone()),
        MockResponse::ok(user_get_with_key(
            22,
            "phasegent-executor",
            "executor-legacy-key",
        )),
        MockResponse::ok(full_list.clone()),
        MockResponse::ok(user_get_with_key(
            33,
            "phasegent-reviewer",
            "reviewer-legacy-key",
        )),
        MockResponse::ok(full_list.clone()),
        MockResponse::ok(user_get_with_key(
            44,
            "phasegent-tester",
            "tester-legacy-key",
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
    // Admin config + credential (legacy role credentials already seeded).
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
    assert_eq!(result.user_memberships.len(), 4);
    let reqs = requests.recv().unwrap();
    assert!(
        !reqs.iter().any(|r| r.starts_with("POST /users.json")),
        "legacy lookup must not create duplicates: {reqs:?}"
    );
    // Stale keys overwritten with admin-read keys.
    assert_eq!(
        storage
            .load_credential(Role::Orchestrator, "redmine")
            .unwrap()
            .as_deref(),
        Some("orchestrator-legacy-key")
    );
    assert_eq!(
        storage.load_redmine_user(Role::Orchestrator).unwrap(),
        Some((11, "phasegent-orchestrator".to_owned()))
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}

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
fn bootstrap_without_tester_credential_still_provisions_tester() {
    let _environment_lock = lock_workflow_tests();
    let (directory, _guard, storage) = temp_db("tester-always");
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
    let (base, requests, server) = sequence(vec![
        MockResponse::error(404, r#"{"errors":["not found"]}"#),
        MockResponse::ok(closed_status()),
        MockResponse::ok(support::project_response(
            44,
            "owner/repo",
            "owner-repo",
            "Workflow",
        )),
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(11, "phasegent-orchestrator")),
        MockResponse::ok(user_get_with_key(
            11,
            "phasegent-orchestrator",
            "orchestrator-key",
        )),
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(22, "phasegent-executor")),
        MockResponse::ok(user_get_with_key(22, "phasegent-executor", "executor-key")),
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(33, "phasegent-reviewer")),
        MockResponse::ok(user_get_with_key(33, "phasegent-reviewer", "reviewer-key")),
        MockResponse::ok(user_list_empty()),
        MockResponse::status(201, user_create_response(44, "phasegent-tester")),
        MockResponse::ok(user_get_with_key(44, "phasegent-tester", "tester-key")),
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
    // Only admin seeded; tester has no prior credential yet Phase 2 must
    // still provision it (admin-only overrides the old optional check).
    seed_admin_only(&storage, &base);
    let result =
        crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None).unwrap();
    assert_eq!(
        result.user_memberships.len(),
        4,
        "tester must always be provisioned"
    );
    assert!(
        result
            .user_memberships
            .iter()
            .any(|m| m.user_id == 44 && m.user_login == "phasegent-tester"),
        "tester membership missing: {:?}",
        result.user_memberships
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
    let reqs = requests.recv().unwrap();
    assert_eq!(reqs.len(), 29, "expected 29 bootstrap requests: {reqs:?}");
}
