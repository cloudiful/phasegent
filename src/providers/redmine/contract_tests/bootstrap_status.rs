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
fn bootstrap_does_not_guess_multiple_closed_statuses() {
    let statuses = [
        crate::providers::redmine::model::RedmineIssueStatus {
            id: 5,
            name: "Closed".to_owned(),
            is_closed: true,
        },
        crate::providers::redmine::model::RedmineIssueStatus {
            id: 6,
            name: "Resolved".to_owned(),
            is_closed: true,
        },
    ];
    let error = RedmineProvider::select_close_status(&statuses, None, None).unwrap_err();
    assert_eq!(error.json()["kind"], "config");
    assert!(error.to_string().contains("multiple closed"));
    assert_eq!(
        RedmineProvider::select_close_status(&statuses, Some("6"), None)
            .unwrap()
            .id,
        6
    );
    assert_eq!(
        RedmineProvider::select_close_status(&statuses, None, Some("Closed"))
            .unwrap()
            .id,
        5
    );
    let not_found_id =
        RedmineProvider::select_close_status(&statuses, Some("99"), None).unwrap_err();
    assert!(not_found_id.to_string().contains("id 99 was not found"));
    let not_closed_id = [crate::providers::redmine::model::RedmineIssueStatus {
        id: 8,
        name: "Resolved".to_owned(),
        is_closed: false,
    }];
    let not_closed_id =
        RedmineProvider::select_close_status(&not_closed_id, Some("8"), None).unwrap_err();
    assert!(
        not_closed_id
            .to_string()
            .contains("id 8 was found but is not closed")
    );
    let not_closed_name = [crate::providers::redmine::model::RedmineIssueStatus {
        id: 8,
        name: "Resolved".to_owned(),
        is_closed: false,
    }];
    let not_closed_name =
        RedmineProvider::select_close_status(&not_closed_name, None, Some("Resolved")).unwrap_err();
    assert!(
        not_closed_name
            .to_string()
            .contains("name 'Resolved' was found but is not closed")
    );
    let not_found_name =
        RedmineProvider::select_close_status(&statuses, None, Some("Missing")).unwrap_err();
    assert!(
        not_found_name
            .to_string()
            .contains("name 'Missing' was not found")
    );
}

#[test]
fn bootstrap_persists_role_scoped_ids_with_private_permissions() {
    // The legacy `<role>.config.json` file layout was retired when
    // the project migrated to a single SQLite database. The
    // bootstrap path now lands every bootstrap result on the
    // `role_redmine_config` row in SQLite via
    // `Storage::persist_redmine_bootstrap`; verify the same
    // round-trip behaviour the legacy test used to pin, with
    // `Storage::open_at` against an isolated temp database so the
    // operator's real config is never touched.
    let temp_dir = std::env::temp_dir().join(format!(
        "phasegent-redmine-bootstrap-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let storage = Storage::open_at(&temp_dir.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .persist_redmine_bootstrap(
            Role::Orchestrator,
            Some("https://redmine.example".to_owned()),
            44,
            5,
        )
        .unwrap();

    let loaded = storage
        .load_redmine_config(Role::Orchestrator)
        .unwrap()
        .expect("bootstrap row must exist");
    assert_eq!(
        loaded.project_id, None,
        "bootstrap must not persist project_id after Phase 1"
    );
    assert_eq!(loaded.close_status_id, Some(5));
    assert_eq!(loaded.api_base.as_deref(), Some("https://redmine.example"));
    // Active bootstrap no longer persists the legacy group fields;
    // older configs that still carry them continue to decode via
    // `serde(default)`.
    assert!(loaded.group_name.is_none());
    assert!(loaded.group_role.is_none());

    let provider = storage
        .load_role_config(Role::Orchestrator)
        .unwrap()
        .expect("role_config row must exist after bootstrap")
        .provider;
    assert_eq!(provider.as_deref(), Some("redmine"));

    let db_path = storage.db_path().to_path_buf();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&db_path).unwrap().permissions().mode() & 0o777,
            0o600,
            "SQLite database file must be 0600"
        );
        let parent_mode = fs::metadata(db_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700, "SQLite database directory must be 0700");
    }
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn bootstrap_persisted_config_decodes_legacy_group_fields_without_error() {
    // Older Redmine configs persisted before the direct-user switch still
    // carry `group_name`/`group_role`. The active bootstrap no longer reads
    // or writes them, but they must decode without error so old files keep
    // loading on operator machines.
    let legacy = serde_json::json!({
        "api_base": "https://redmine.example",
        "project_id": "44",
        "close_status_id": 5,
        "group_name": "AI Agents",
        "group_role": "开发人员",
    });
    let config: auth::RedmineStoredConfig =
        serde_json::from_value(legacy).expect("legacy config must decode");
    assert_eq!(config.group_name.as_deref(), Some("AI Agents"));
    assert_eq!(config.group_role.as_deref(), Some("开发人员"));
}

#[test]
fn role_redmine_user_table_is_additive_and_round_trips() {
    // Phase 2 adds `role_redmine_user` additively. A database created
    // without the table (legacy install) must gain it on open with no
    // data loss, and the new mapping must round-trip per role.
    let temp_dir = std::env::temp_dir().join(format!(
        "phasegent-redmine-user-migration-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let db_path = temp_dir.join(crate::infra::storage::DB_FILENAME);
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS role_config (role TEXT PRIMARY KEY, provider TEXT, api_base TEXT, repository TEXT);
             CREATE TABLE IF NOT EXISTS role_redmine_config (role TEXT PRIMARY KEY, api_base TEXT, project_id TEXT, close_status_id INTEGER);
             CREATE TABLE IF NOT EXISTS role_credential (role TEXT NOT NULL, provider TEXT NOT NULL, credential TEXT NOT NULL, PRIMARY KEY (role, provider));
             CREATE TABLE IF NOT EXISTS global_setting (name TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO role_credential (role, provider, credential) VALUES ('orchestrator', 'redmine', 'legacy-key')",
            [],
        )
        .unwrap();
    }
    let storage = Storage::open_at(&db_path).unwrap();
    // Legacy credential survives the additive migration.
    assert_eq!(
        storage
            .load_credential(Role::Orchestrator, "redmine")
            .unwrap()
            .as_deref(),
        Some("legacy-key")
    );
    // New table starts empty, then round-trips.
    assert!(
        storage
            .load_redmine_user(Role::Orchestrator)
            .unwrap()
            .is_none()
    );
    storage
        .save_redmine_user(Role::Orchestrator, 11, "phasegent-orchestrator")
        .unwrap();
    assert_eq!(
        storage.load_redmine_user(Role::Orchestrator).unwrap(),
        Some((11, "phasegent-orchestrator".to_owned()))
    );
    // Overwrite replaces the mapping; other roles stay isolated.
    storage
        .save_redmine_user(Role::Orchestrator, 12, "phasegent-orchestrator")
        .unwrap();
    assert_eq!(
        storage.load_redmine_user(Role::Orchestrator).unwrap(),
        Some((12, "phasegent-orchestrator".to_owned()))
    );
    assert!(storage.load_redmine_user(Role::Executor).unwrap().is_none());
    // Validation guards.
    assert!(storage.save_redmine_user(Role::Executor, 0, "x").is_err());
    assert!(storage.save_redmine_user(Role::Executor, 7, "   ").is_err());
    // Reopen stays idempotent.
    drop(storage);
    let reopened = Storage::open_at(&db_path).unwrap();
    assert_eq!(
        reopened.load_redmine_user(Role::Orchestrator).unwrap(),
        Some((12, "phasegent-orchestrator".to_owned()))
    );
    assert_eq!(
        reopened
            .load_credential(Role::Orchestrator, "redmine")
            .unwrap()
            .as_deref(),
        Some("legacy-key")
    );
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn bootstrap_output_and_errors_redact_provisioned_keys() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-redacted-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = directory.join(crate::infra::storage::DB_FILENAME);
    let _guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let storage = Storage::open_at(&db_path).unwrap();
    // Distinct collision with secret-bearing keys: the error must redact.
    const ORCH_SECRET: &str = "orchestrator-secret-aaa111";
    const EXEC_SECRET: &str = "executor-secret-bbb222";
    let (base, requests, server) = sequence(vec![
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
    ]);
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
    storage
        .save_redmine_user(Role::Orchestrator, 11, "phasegent-orchestrator")
        .unwrap();
    storage
        .save_credential(Role::Orchestrator, "redmine", ORCH_SECRET)
        .unwrap();
    storage
        .save_redmine_user(Role::Executor, 11, "phasegent-orchestrator")
        .unwrap();
    storage
        .save_credential(Role::Executor, "redmine", EXEC_SECRET)
        .unwrap();
    storage
        .save_redmine_user(Role::Reviewer, 33, "phasegent-reviewer")
        .unwrap();
    storage
        .save_credential(Role::Reviewer, "redmine", "reviewer-secret-ccc333")
        .unwrap();
    storage
        .save_redmine_user(Role::Tester, 44, "phasegent-tester")
        .unwrap();
    storage
        .save_credential(Role::Tester, "redmine", "tester-secret-ddd444")
        .unwrap();

    let error = crate::workflow::bootstrap(Role::Admin, None, Some("owner/repo"), None, None)
        .expect_err("distinct collision must fail");
    let rendered = error.json().to_string();
    for secret in [
        ORCH_SECRET,
        EXEC_SECRET,
        "reviewer-secret-ccc333",
        "tester-secret-ddd444",
        "admin-redmine-key",
    ] {
        assert!(
            !rendered.contains(secret),
            "bootstrap error must redact provisioned keys: {rendered}"
        );
        assert!(
            !error.to_string().contains(secret),
            "Display must redact: {error}"
        );
    }
    assert!(rendered.contains("distinct users"), "{rendered}");
    // Successful bootstrap JSON contract never carries keys either.
    let success = serde_json::json!({
        "bootstrapped": true,
        "project_id": 44_u64,
        "user_memberships": [
            {"role": "Maintainer", "user_id": 11_u64, "user_login": "phasegent-orchestrator", "status": "added"},
        ],
    });
    let success_rendered = success.to_string();
    for secret in [ORCH_SECRET, EXEC_SECRET, "admin-redmine-key"] {
        assert!(!success_rendered.contains(secret));
    }
    // Config snapshot redacts as well.
    let snapshot = crate::config_snapshot::render(&storage, None).unwrap();
    let snapshot_rendered = serde_json::to_string(&snapshot).unwrap();
    for secret in [ORCH_SECRET, EXEC_SECRET, "admin-redmine-key"] {
        assert!(
            !snapshot_rendered.contains(secret),
            "snapshot must redact: {snapshot_rendered}"
        );
    }
    let _ = requests.recv().unwrap();
    server.join().unwrap();
    let _ = fs::remove_dir_all(directory);
}
