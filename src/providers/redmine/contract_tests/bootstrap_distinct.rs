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
use crate::command::{
    self, Command, IssueCommand, ProjectCommand, RelationCommand, StatusCommand, WorkflowCommand,
};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{Storage, TimerRun};
use crate::policy::{Capability, Role};
use crate::providers::redmine::model::{RedmineRelationType, RedmineTimeEntryActivity};
use crate::providers::{
    ProviderDispatcher, ProviderKind, RedmineConfig, RedmineIssueStatus, RedmineMetadataProvider,
    RedmineProvider,
};
use std::str::FromStr;
use std::{fs, time};

#[test]
fn bootstrap_fails_with_distinct_users_error_when_two_keys_resolve_to_same_user() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-distinct-users-{}-{}",
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
        .save_credential(Role::Orchestrator, "redmine", "orchestrator-redmine-key")
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

    let (base, requests, server) = sequence(vec![
        // Admin-side project bootstrap gets us to the identity lookup phase.
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
        // orchestrator identity resolves to user id 11.
        MockResponse::ok(support::current_user_response(11, "shared-user")),
        // executor identity ALSO resolves to user id 11 — the collision the
        // distinct-users check is supposed to catch.
        MockResponse::ok(support::current_user_response(11, "shared-user")),
        // reviewer identity still gets fetched before the check fires so all
        // three pairwise comparisons have data.
        MockResponse::ok(support::current_user_response(33, "reviewer")),
    ]);
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

    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("bootstrap must fail when two role keys resolve to the same Redmine user");
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
        message.contains("shared-user") || message.contains("#11"),
        "error message must describe the colliding identity, got: {message}"
    );

    // Bootstrap must abort after the three current_user lookups and never
    // issue a membership POST/PUT — otherwise the partial mapping would leak
    // into the project.
    let observed_requests = requests.recv().unwrap();
    assert_eq!(
        observed_requests.len(),
        6,
        "bootstrap must stop after the three current_user lookups on distinct-user failure"
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
    }

    // Project bootstrap config must not be persisted on the admin role: the
    // workflow is not ready and we must not leave a partial identity mapping
    // behind for the next operator run to discover. The seeded api_base
    // remains in place because the bootstrap returned early before
    // `persist_redmine_bootstrap` could run, but the project id and
    // close status must NOT have been written.
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
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-tester-distinct-{}-{}",
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
        .save_credential(Role::Orchestrator, "redmine", "orchestrator-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Executor, "redmine", "executor-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Reviewer, "redmine", "reviewer-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Tester, "redmine", "tester-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();

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
        MockResponse::ok(support::current_user_response(11, "orchestrator")),
        MockResponse::ok(support::current_user_response(22, "executor")),
        MockResponse::ok(support::current_user_response(33, "reviewer")),
        MockResponse::ok(support::current_user_response(
            22,
            "tester-collides-executor",
        )),
    ]);
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
        7,
        "must stop after 4 current_user lookups on tester collision"
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
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-tester-ok-{}-{}",
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
        .save_credential(Role::Orchestrator, "redmine", "orchestrator-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Executor, "redmine", "executor-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Reviewer, "redmine", "reviewer-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Tester, "redmine", "tester-redmine-key")
        .unwrap();
    storage
        .save_credential(Role::Admin, "redmine", "admin-redmine-key")
        .unwrap();
    let _mirror_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "mirror-bearer-key");
    let _mirror_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://git.example.com/owner/repo.git",
    );
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
        MockResponse::ok(support::current_user_response(11, "orchestrator")),
        MockResponse::ok(support::current_user_response(22, "executor")),
        MockResponse::ok(support::current_user_response(33, "reviewer")),
        MockResponse::ok(support::current_user_response(44, "tester")),
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
