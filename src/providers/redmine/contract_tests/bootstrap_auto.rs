#![allow(unused_imports)]
use super::support;
use super::support::*;
use crate::auth;
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use std::{fs, time};

#[test]
fn issue_create_automatically_bootstraps_once_before_returning_issue() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-auto-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = directory.join(crate::infra::storage::DB_FILENAME);
    let _db_path_guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", TEST_API_KEY)
        .unwrap();
    storage
        .save_credential(Role::Executor, "redmine", "executor-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Reviewer, "redmine", "reviewer-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );

    let (base, requests, server) = sequence(vec![
        // Bootstrap sequence (admin provider): project lookup, statuses,
        // project create, then three `/users/current.json` lookups
        // (orchestrator/executor/reviewer role-scoped keys), then for each
        // of the three agent users: role list, membership list, and
        // membership POST (admin key).
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
        // 3: orchestrator identity (orchestrator-scoped key)
        MockResponse::ok(support::current_user_response(11, "orchestrator")),
        // 4: executor identity (executor-scoped key)
        MockResponse::ok(support::current_user_response(22, "executor")),
        // 5: reviewer identity (reviewer-scoped key)
        MockResponse::ok(support::current_user_response(33, "reviewer")),
        // 6-8: orchestrator reconcile (admin key)
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        // 9-11: executor reconcile (admin key)
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        // 12-14: reviewer reconcile (admin key)
        MockResponse::ok(support::role_collection(&[
            (3, "Maintainer"),
            (4, "Developer"),
            (5, "Reporter"),
        ])),
        MockResponse::ok(support::membership_collection(None)),
        MockResponse::ok("{}"),
        // 15: mirror plugin GET (404 → triggers POST)
        MockResponse::error(404, r#"{"errors":["mirror not found"]}"#),
        // 16: mirror plugin POST (202 → queued)
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
        // First issue create (orchestrator key)
        MockResponse::ok(support::issue_response(80, "Created", "Body", false, &[])),
        // Second issue create (orchestrator key, bootstrap result reused)
        MockResponse::ok(support::issue_response(
            81,
            "Created again",
            "Body",
            false,
            &[],
        )),
        // Issue search (orchestrator key)
        MockResponse::ok(support::issue_collection(1, 100, &[(80, "Created", false)])),
        // Explicit project id: bypasses bootstrap
        MockResponse::ok(support::issue_response(82, "Explicit", "Body", false, &[])),
    ]);
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
    assert_eq!(requests.len(), 21);
    // Bootstrap request order:
    //   0: project lookup (admin)
    //   1: status list (admin)
    //   2: project create (admin)
    //   3: orchestrator current user (orchestrator key)
    //   4: executor current user (executor key)
    //   5: reviewer current user (reviewer key)
    //   6: role list (admin)
    //   7: membership list (admin)
    //   8: orchestrator membership POST (admin)
    //   9: role list (admin)
    //  10: membership list (admin)
    //  11: executor membership POST (admin)
    //  12: role list (admin)
    //  13: membership list (admin)
    //  14: reviewer membership POST (admin)
    //  15: mirror plugin GET (mirror key)
    //  16: mirror plugin POST (mirror key)
    //  17: first issue create (orchestrator)
    //  18: second issue create (orchestrator)
    //  19: issue search (orchestrator)
    //  20: explicit project id issue create (orchestrator)
    support::assert_request_with_key(
        &requests[0],
        "GET",
        "/projects/owner-repo.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[1],
        "GET",
        "/issue_statuses.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[2],
        "POST",
        "/projects.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[3],
        "GET",
        "/users/current.json",
        None,
        TEST_API_KEY,
    );
    support::assert_request_with_key(
        &requests[4],
        "GET",
        "/users/current.json",
        None,
        "executor-redmine-key",
    );
    support::assert_request_with_key(
        &requests[5],
        "GET",
        "/users/current.json",
        None,
        "reviewer-redmine-key",
    );
    support::assert_request_with_key(
        &requests[6],
        "GET",
        "/roles.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[7],
        "GET",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[8],
        "POST",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[9],
        "GET",
        "/roles.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[10],
        "GET",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[11],
        "POST",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[12],
        "GET",
        "/roles.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[13],
        "GET",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    support::assert_request_with_key(
        &requests[14],
        "POST",
        "/projects/44/memberships.json",
        None,
        "admin-redmine-key",
    );
    // Orchestrator membership uses Maintainer (id 3) for user 11.
    assert!(requests[8].contains(r#""user_id":11,"role_ids":[3]"#));
    // Executor membership uses Developer (id 4) for user 22.
    assert!(requests[11].contains(r#""user_id":22,"role_ids":[4]"#));
    // Reviewer membership uses Reporter (id 5) for user 33.
    assert!(requests[14].contains(r#""user_id":33,"role_ids":[5]"#));
    // Mirror plugin GET uses the bearer key on the plugin path.
    support::assert_request_with_bearer(
        &requests[15],
        "GET",
        "/sys/redmine_git_mirror/projects/44/repository/mirror_44_owner_repo",
        None,
        "mirror-bearer-key",
    );
    // Mirror plugin POST carries the JSON `{ "url": ... }` body and bearer key.
    support::assert_request_with_bearer(
        &requests[16],
        "POST",
        "/sys/redmine_git_mirror/projects/44/repository",
        Some(r#""url":"https://git.example.com/owner/repo.git""#),
        "mirror-bearer-key",
    );
    support::assert_request(&requests[17], "POST", "/issues.json", None);
    support::assert_request(&requests[18], "POST", "/issues.json", None);
    support::assert_request(&requests[19], "GET", "/issues.json?", None);
    support::assert_request(&requests[20], "POST", "/issues.json", None);
    assert_eq!(
        storage
            .load_credential(Role::Orchestrator, "redmine")
            .unwrap()
            .expect("orchestrator credential must remain after bootstrap"),
        TEST_API_KEY
    );
    assert!(requests[20].contains(r#""project_id":99"#));
    let stored = auth::load_redmine_config(Role::Orchestrator, &storage)
        .unwrap()
        .unwrap();
    assert_eq!(stored.project_id.as_deref(), Some("44"));
    assert_eq!(stored.close_status_id, Some(5));
    // Active bootstrap no longer persists the legacy group fields.
    assert_eq!(stored.group_name, None);
    assert_eq!(stored.group_role, None);
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
