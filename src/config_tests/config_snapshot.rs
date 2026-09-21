use super::support::*;
use super::*;

/// The `config show` snapshot contract after project-id removal: the snapshot
/// names `gitlab_api_base`/`gitlab_credential` and reports credential
/// presence/length only (never a secret), an unset credential omits the length
/// slot entirely, no `gitlab_project_id`/`redmine_project_id` slot may come
/// back, legacy persisted project ids stay inert in storage, and provider
/// resolution ignores both the legacy rows and the legacy environment
/// variables while an explicit project id keeps winning.
#[test]
fn config_snapshot_omits_project_id_surface_and_legacy_values_stay_inert() {
    with_isolated_storage("show-project-id-contract", |db_path, storage| {
        // Fresh storage: unset fields stay absent and no credential slot is
        // rendered with a length.
        let empty = config::show(Some(Role::Executor), storage).unwrap();
        let empty_text = serde_json::to_string(&empty).unwrap();
        let empty_roles = empty["roles"].as_array().expect("roles array");
        assert_eq!(empty_roles.len(), 1);
        let empty_executor = &empty_roles[0];
        assert!(empty_executor["gitlab_api_base"].is_null());
        for removed in ["gitlab_project_id", "redmine_project_id"] {
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
            empty_executor["gitlab_credential"]["present"],
            Value::Bool(false)
        );
        assert!(
            empty_executor["gitlab_credential"]["length"].is_null(),
            "zero-length credential summary must omit the length slot: {empty_executor:?}"
        );
        assert!(
            empty_text.contains("gitlab_api_base"),
            "snapshot must name gitlab_api_base: {empty_text}"
        );
        assert!(
            empty_text.contains("gitlab_credential"),
            "snapshot must name gitlab_credential: {empty_text}"
        );

        storage
            .save_credential(Role::Executor, PROVIDER_GITLAB, "gitlab-private-token-shhh")
            .unwrap();
        storage
            .save_gitlab_config(
                Role::Executor,
                &crate::auth::GitlabStoredConfig {
                    api_base: Some("https://gitlab.example".to_owned()),
                    project_id: Some(42),
                },
            )
            .unwrap();
        storage
            .save_credential(Role::Executor, PROVIDER_FORGEJO, "forgejo-secret-token")
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
        assert_eq!(
            executor["gitlab_api_base"].as_str(),
            Some("https://gitlab.example")
        );
        // Project-id fields were removed; stored values are ignored and
        // must not appear in the snapshot.
        assert!(
            executor.get("gitlab_project_id").is_none(),
            "snapshot must not expose gitlab_project_id after Phase 1: {executor:?}"
        );
        assert!(
            executor.get("redmine_project_id").is_none(),
            "snapshot must not expose redmine_project_id after Phase 1: {executor:?}"
        );
        assert_eq!(executor["gitlab_credential"]["present"], Value::Bool(true));
        assert_eq!(
            executor["gitlab_credential"]["length"],
            Value::from("gitlab-private-token-shhh".len())
        );
        assert_eq!(executor["forgejo_credential"]["present"], Value::Bool(true));
        assert_eq!(executor["redmine_credential"]["present"], Value::Bool(true));
        // Verify legacy stored project_id was ignored, not leaked.
        let stored = storage.load_gitlab_config(Role::Executor).unwrap().unwrap();
        assert_eq!(
            stored.project_id, None,
            "legacy gitlab project_id must be inert (load returns None)"
        );

        // Simulate a legacy database where project ids were persisted by
        // writing directly via SQL before the migration runs.
        storage
            .connection
            .execute(
                "INSERT INTO role_redmine_config (role, api_base, project_id, close_status_id) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(role) DO UPDATE SET api_base=excluded.api_base, project_id=excluded.project_id, close_status_id=excluded.close_status_id",
                rusqlite::params!["executor", "https://redmine.example", "legacy-redmine-id", 5_i64],
            )
            .unwrap();
        storage
            .connection
            .execute(
                "INSERT INTO role_gitlab_config (role, api_base, project_id) VALUES (?1, ?2, ?3) ON CONFLICT(role) DO UPDATE SET api_base=excluded.api_base, project_id=excluded.project_id",
                rusqlite::params!["executor", "https://gitlab.example", 99_i64],
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
        let gitlab = reopened
            .load_gitlab_config(Role::Executor)
            .unwrap()
            .unwrap();
        assert_eq!(
            gitlab.project_id, None,
            "gitlab legacy project_id must be inert"
        );
        assert_eq!(gitlab.api_base.as_deref(), Some("https://gitlab.example"));
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
        let gitlab_raw: Option<i64> = reopened
            .connection
            .query_row(
                "SELECT project_id FROM role_gitlab_config WHERE role='executor'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            gitlab_raw.is_none(),
            "raw gitlab project_id column must be NULL: {gitlab_raw:?}"
        );

        // Provider resolution must not use legacy values: Redmine without
        // explicit --project-id must have None, GitLab without explicit
        // must error even though legacy row existed.
        let _env_redmine = EnvGuard::set("PHASEGENT_REDMINE_PROJECT_ID", "env-id");
        let _env_gitlab = EnvGuard::set("PHASEGENT_GITLAB_PROJECT_ID", "123");
        let _env_generic = EnvGuard::set("PHASEGENT_PROJECT_ID", "generic-id");
        let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
        // Redmine: explicit None, env present, but env is ignored.
        let redmine_config = crate::providers::config::RedmineConfig::resolve(
            Role::Executor,
            Some("https://redmine.example"),
            None,
            Some("5"),
        )
        .unwrap();
        assert_eq!(
            redmine_config.project_id, None,
            "redmine env must be ignored after Phase 1"
        );
        // GitLab: explicit None, env present, must still error.
        let gitlab_err = crate::providers::config::GitlabConfig::resolve(
            Role::Executor,
            Some("https://gitlab.example"),
            None,
        )
        .unwrap_err();
        assert!(
            gitlab_err.to_string().contains("not configured"),
            "gitlab must require explicit project-id: {gitlab_err}"
        );
        // Explicit project-id still wins.
        let redmine_explicit = crate::providers::config::RedmineConfig::resolve(
            Role::Executor,
            Some("https://redmine.example"),
            Some("explicit-42"),
            Some("5"),
        )
        .unwrap();
        assert_eq!(redmine_explicit.project_id.as_deref(), Some("explicit-42"));
        let gitlab_explicit = crate::providers::config::GitlabConfig::resolve(
            Role::Executor,
            Some("https://gitlab.example"),
            Some("77"),
        )
        .unwrap();
        assert_eq!(gitlab_explicit.project_id, 77);
    });
}
