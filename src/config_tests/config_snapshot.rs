use super::support::*;
use super::*;

/// The `config show` snapshot contract after provider removal: the
/// snapshot reports the role provider, the role-scoped Redmine
/// close-status id, and credential presence/length only (never a
/// secret), and no retired-provider field may come back. Legacy persisted
/// project ids stay inert in storage, and Redmine provider resolution
/// ignores the legacy project-id environment variables while an explicit
/// project id keeps winning.
#[test]
fn config_snapshot_omits_retired_provider_surface_and_legacy_values_stay_inert() {
    with_isolated_storage("show-provider-contract", |db_path, storage| {
        // Fresh storage: unset fields stay absent and no credential slot is
        // rendered with a length.
        let empty = config::show(Some(Role::Executor), storage).unwrap();
        let empty_text = serde_json::to_string(&empty).unwrap();
        let empty_roles = empty["roles"].as_array().expect("roles array");
        assert_eq!(empty_roles.len(), 1);
        let empty_executor = &empty_roles[0];
        for removed in [
            "gitlab_project_id",
            "redmine_project_id",
            "gitlab_api_base",
            "forgejo_api_base",
            "forgejo_repository",
            "forgejo_credential",
            "gitlab_credential",
        ] {
            assert!(
                empty_executor.get(removed).is_none(),
                "snapshot must not expose {removed}: {empty_executor:?}"
            );
            assert!(
                !empty_text.contains(removed),
                "snapshot must not contain {removed}: {empty_text}"
            );
        }
        assert_eq!(
            empty_executor["redmine_credential"]["present"],
            Value::Bool(false)
        );
        assert!(
            empty_executor["redmine_credential"]["length"].is_null(),
            "zero-length credential summary must omit the length slot: {empty_executor:?}"
        );
        assert!(
            empty_text.contains("redmine_credential"),
            "snapshot must name redmine_credential: {empty_text}"
        );
        assert!(
            empty_text.contains("redmine_close_status_id"),
            "snapshot must name redmine_close_status_id: {empty_text}"
        );

        // A retired-provider credential row may still exist in an operator
        // database; it must never surface as a field or leak its value.
        storage
            .save_credential(Role::Executor, "gitlab", "gitlab-private-token-shhh")
            .unwrap();
        storage
            .save_credential(Role::Executor, "forgejo", "forgejo-secret-token")
            .unwrap();
        storage
            .save_credential(Role::Executor, PROVIDER_REDMINE, "redmine-secret-key")
            .unwrap();

        let snapshot = config::show(Some(Role::Executor), storage).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();

        for forbidden in [
            "gitlab-private-token-shhh",
            "forgejo-secret-token",
            "redmine-secret-key",
        ] {
            assert!(
                !text.contains(forbidden),
                "snapshot leaked '{forbidden}': {text}"
            );
        }

        let roles = snapshot["roles"].as_array().expect("roles array");
        assert_eq!(roles.len(), 1);
        let executor = &roles[0];
        assert_eq!(executor["role"], "executor");
        assert_eq!(executor["redmine_credential"]["present"], Value::Bool(true));
        assert_eq!(
            executor["redmine_credential"]["length"],
            Value::from("redmine-secret-key".len())
        );

        // Simulate a legacy database where a project id was persisted by
        // writing directly via SQL before the migration runs.
        storage
            .connection
            .execute(
                "INSERT INTO role_redmine_config (role, api_base, project_id, close_status_id) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(role) DO UPDATE SET api_base=excluded.api_base, project_id=excluded.project_id, close_status_id=excluded.close_status_id",
                rusqlite::params!["executor", "https://redmine.example", "legacy-redmine-id", 5_i64],
            )
            .unwrap();
        // Re-open to trigger the migration that clears legacy values.
        let reopened = Storage::open_at(db_path).unwrap();
        let redmine = reopened
            .load_redmine_config(Role::Executor)
            .unwrap()
            .unwrap();
        assert_eq!(
            redmine.project_id, None,
            "redmine legacy project_id must be inert"
        );
        assert_eq!(redmine.api_base.as_deref(), Some("https://redmine.example"));
        assert_eq!(redmine.close_status_id, Some(5));
        // Verify raw column is NULL after migration.
        let redmine_raw: Option<String> = reopened
            .connection
            .query_row(
                "SELECT project_id FROM role_redmine_config WHERE role='executor'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            redmine_raw.is_none(),
            "raw redmine project_id column must be NULL: {redmine_raw:?}"
        );

        // Provider resolution must not use legacy/env project values: an
        // explicit --project-id wins, and the legacy environment variables
        // are ignored.
        let _env_redmine = EnvGuard::set("PHASEGENT_REDMINE_PROJECT_ID", "env-id");
        let _env_generic = EnvGuard::set("PHASEGENT_PROJECT_ID", "generic-id");
        let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
        let redmine_config = crate::providers::config::RedmineConfig::resolve(
            Role::Executor,
            Some("https://redmine.example"),
            None,
            Some("5"),
        )
        .unwrap();
        assert_eq!(
            redmine_config.project_id, None,
            "redmine env must be ignored"
        );
        let redmine_explicit = crate::providers::config::RedmineConfig::resolve(
            Role::Executor,
            Some("https://redmine.example"),
            Some("explicit-42"),
            Some("5"),
        )
        .unwrap();
        assert_eq!(redmine_explicit.project_id.as_deref(), Some("explicit-42"));
    });
}
