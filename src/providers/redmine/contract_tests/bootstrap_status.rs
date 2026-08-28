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
use crate::ci_model::CiRunsFilter;
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
    assert_eq!(loaded.project_id.as_deref(), Some("44"));
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
