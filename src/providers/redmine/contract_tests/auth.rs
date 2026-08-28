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
fn parser_auth_config_and_provider_selection_regressions() {
    let admin = command::parse(&strings([
        "--role",
        "admin",
        "--provider",
        "redmine",
        "project",
        "list",
    ]))
    .unwrap();
    assert_eq!(admin.role, Some(Role::Admin));
    let invalid_role = "invalid".parse::<Role>().unwrap_err();
    assert!(invalid_role.contains("admin, orchestrator, executor, or reviewer"));

    let args = strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "issue",
        "search",
        "--query",
        "needle",
        "--state",
        "closed",
    ]);
    let invocation = command::parse(&args).unwrap();
    assert_eq!(invocation.provider, Some(ProviderKind::Redmine));
    assert!(matches!(
        invocation.command,
        Command::Issue(IssueCommand::Search { ref query, ref state })
            if query.as_deref() == Some("needle") && state == "closed"
    ));

    let auth_args = strings([
        "--role",
        "orchestrator",
        "--provider",
        "redmine",
        "auth",
        "setup",
        "--stdin",
        "--api-base",
        "https://redmine.example",
        "--project-id",
        "42",
        "--close-status-id",
        "37",
    ]);
    assert!(matches!(
        command::parse(&auth_args).unwrap().command,
        Command::AuthSetup {
            read_stdin: true,
            provider: None,
            ref api_base,
            ref project_id,
            ref close_status_id,
            repository: None,
        } if api_base.as_deref() == Some("https://redmine.example")
            && project_id.as_deref() == Some("42")
            && close_status_id.as_deref() == Some("37")
    ));

    let config = RedmineConfig::new("https://redmine.example/", "42", 37);
    assert_eq!(config.provider(), ProviderKind::Redmine);
    assert_eq!(config.require_project_id().unwrap(), "42");
    assert_eq!(config.require_close_status_id().unwrap(), 37);
    assert_eq!(
        ProviderKind::from_str("redmine").unwrap(),
        ProviderKind::Redmine
    );
    assert_eq!(
        crate::providers::config::resolve_kind(Role::Reviewer, Some(ProviderKind::Redmine))
            .unwrap(),
        ProviderKind::Redmine
    );

    let key_path =
        std::path::Path::new("/tmp/phasegent-test").join(crate::infra::storage::DB_FILENAME);
    assert!(key_path.ends_with("phasegent.sqlite3"));
    assert_eq!(
        auth::setup_provider(
            Role::Orchestrator,
            "redmine",
            auth::SetupOptions {
                read_stdin: false,
                api_base: None,
                repository: Some("owner/repo".to_owned()),
                project_id: None,
                close_status_id: None,
            },
        )
        .unwrap_err(),
        "--repository requires the forgejo provider"
    );
}

#[test]
fn admin_auth_setup_writes_the_normal_role_scoped_private_key() {
    // The legacy `<role>.key` file layout was retired when the
    // project migrated to a single SQLite database. The admin role
    // now persists its Redmine API key in the `role_credential`
    // table; verify the round-trip and the file-mode invariant
    // (the SQLite database file is 0600) the legacy test used to
    // pin. The temp database is opened via `Storage::open_at` so
    // the operator's real config is never touched.
    let temp_dir = std::env::temp_dir().join(format!(
        "phasegent-redmine-admin-key-{}-{}",
        std::process::id(),
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let secret = "admin-secret";
    let storage = Storage::open_at(&temp_dir.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .save_credential(Role::Admin, "redmine", secret)
        .unwrap();

    let loaded = storage
        .load_credential(Role::Admin, "redmine")
        .unwrap()
        .expect("admin redmine credential must exist after save");
    assert_eq!(loaded, secret);

    let other = storage
        .load_credential(Role::Orchestrator, "redmine")
        .unwrap();
    assert!(
        other.is_none(),
        "orchestrator must not observe admin's redmine credential"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let db_path = storage.db_path();
        assert_eq!(
            fs::metadata(db_path).unwrap().permissions().mode() & 0o777,
            0o600,
            "SQLite database file must be 0600"
        );
    }
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn admin_provider_requires_admin_key_without_falling_back() {
    let _environment_lock = lock_workflow_tests();
    let directory = std::env::temp_dir().join(format!(
        "phasegent-redmine-missing-admin-{}-{}",
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
        .save_credential(Role::Orchestrator, "redmine", "normal-secret")
        .unwrap();
    let result = RedmineProvider::for_role(
        Role::Admin,
        RedmineConfig::new("http://redmine.test", "42", 37),
    );
    let error = match result {
        Ok(_) => panic!("admin provider unexpectedly used the orchestrator key"),
        Err(error) => error,
    };
    assert_eq!(error.json()["kind"], "auth");
    assert!(error.to_string().contains("could not read Redmine API key"));
    let _ = fs::remove_dir_all(directory);
}
