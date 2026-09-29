use super::support::*;
use super::*;

#[test]
fn config_set_clear_reject_the_removed_repository_setting() {
    // `PHASEGENT_REPOSITORY` belonged to the removed provider: the
    // canonical name and the `repository` alias are unknown to
    // set/clear at parse time and at dispatch, while rows the storage
    // layer already holds stay intact (no destructive migration).
    assert!(config_write::canonical_setting_name("PHASEGENT_REPOSITORY").is_none());
    assert!(config_write::canonical_setting_name("repository").is_none());
    for setting in ["PHASEGENT_REPOSITORY", "repository"] {
        let args = ["admin", "config", "set", setting, "owner/repo"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let error = command::parse_with_role_env(&args, Some("executor"))
            .expect_err("removed repository setting must not parse for set");
        assert!(error.contains("unknown config setting"), "got: {error}");
        let args = ["admin", "config", "clear", setting]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let error = command::parse_with_role_env(&args, Some("executor"))
            .expect_err("removed repository setting must not parse for clear");
        assert!(error.contains("unknown config setting"), "got: {error}");
    }
    with_isolated_storage("set-clear-repository-removed", |_db_path, storage| {
        let error = config_write::set_setting_value(
            Some(Role::Executor),
            "PHASEGENT_REPOSITORY",
            "owner/repo",
            storage,
        )
        .unwrap_err();
        assert!(error.contains("unknown setting"), "got: {error}");
        // A legacy row with a stored repository survives the removal:
        // clear dispatch rejects the name without touching the row.
        storage
            .save_role_config(
                Role::Executor,
                &crate::auth::StoredConfig {
                    provider: None,
                    api_base: None,
                    repository: Some("owner/repo".to_owned()),
                },
            )
            .unwrap();
        let error =
            config_write::clear_setting(Some(Role::Executor), "PHASEGENT_REPOSITORY", storage)
                .unwrap_err();
        assert!(error.contains("unknown setting"), "got: {error}");
        let loaded = storage.load_role_config(Role::Executor).unwrap().unwrap();
        assert_eq!(loaded.repository.as_deref(), Some("owner/repo"));
    });
}

#[test]
fn config_set_secret_via_stdin_persists_and_show_redacted() {
    with_isolated_storage("set-secret-stdin", |_db_path, storage| {
        let secret = "super-secret-bearer-123";
        let outcome = config_write::set_setting_stdin_content(
            None,
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
            &format!("  {secret}  \n"),
            storage,
        )
        .unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        assert!(text.contains("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY"));
        assert!(
            !text.contains(secret),
            "set outcome must not echo secret: {text}"
        );
        // Persisted value should be trimmed
        let stored = storage
            .load_global_setting("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
            .unwrap()
            .expect("stored");
        assert_eq!(stored, secret);
        let snapshot = config::show(None, storage).unwrap();
        let snap_text = serde_json::to_string(&snapshot).unwrap();
        assert!(
            !snap_text.contains(secret),
            "snapshot leaked secret: {snap_text}"
        );
        let settings = snapshot["global_settings"].as_array().unwrap();
        let entry = settings
            .iter()
            .find(|e| e["name"] == "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
            .unwrap();
        assert_eq!(entry["present"], Value::Bool(true));
        assert_eq!(entry["length"], Value::from(secret.len()));
    });
}

#[test]
fn config_set_role_scoped_persists_and_output_canonical() {
    with_isolated_storage("set-role-scoped", |_db_path, storage| {
        let outcome = config_write::set_setting_value(
            Some(Role::Executor),
            "PHASEGENT_API_BASE",
            "https://redmine.example",
            storage,
        )
        .unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        assert!(text.contains("PHASEGENT_API_BASE"));
        assert!(
            !text.contains("https://redmine.example"),
            "value must not be echoed: {text}"
        );
        // Verify storage: generic api-base writes to three rows
        let generic = storage.load_role_config(Role::Executor).unwrap().unwrap();
        assert_eq!(generic.api_base.as_deref(), Some("https://redmine.example"));
        let redmine = storage
            .load_redmine_config(Role::Executor)
            .unwrap()
            .unwrap();
        assert_eq!(redmine.api_base.as_deref(), Some("https://redmine.example"));
        let gitlab = storage.load_gitlab_config(Role::Executor).unwrap().unwrap();
        assert_eq!(gitlab.api_base.as_deref(), Some("https://redmine.example"));

        // Project-id aliases are now rejected; verify they do not persist.
        assert!(config_write::canonical_setting_name("redmine-project-id").is_none());
        assert!(config_write::canonical_setting_name("gitlab-project-id").is_none());
        assert!(config_write::canonical_setting_name("project-id").is_none());
    });
}

#[test]
fn config_set_default_provider_reuses_validation() {
    with_isolated_storage("set-default-provider", |_db_path, storage| {
        for literal in [PROVIDER_REDMINE, PROVIDER_GITLAB] {
            let outcome = config_write::set_setting_value(
                None,
                "PHASEGENT_DEFAULT_PROVIDER",
                literal,
                storage,
            )
            .unwrap();
            let text = serde_json::to_string(&outcome).unwrap();
            assert!(text.contains("PHASEGENT_DEFAULT_PROVIDER"));
            assert!(!text.contains(literal));
            let stored = storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .unwrap();
            assert_eq!(stored, literal);
        }
        let err =
            config_write::set_setting_value(None, "PHASEGENT_DEFAULT_PROVIDER", "wrong", storage)
                .unwrap_err();
        assert!(err.contains("invalid provider"), "got: {err}");
        assert!(err.contains("wrong"), "got: {err}");
    });
}

#[test]
fn config_clear_global_without_role_and_role_scoped() {
    with_isolated_storage("clear", |_db_path, storage| {
        storage
            .save_global_setting("PHASEGENT_REDMINE_REPOSITORY_URL", "https://example.com")
            .unwrap();
        let outcome =
            config_write::clear_setting(None, "PHASEGENT_REDMINE_REPOSITORY_URL", storage).unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        assert!(text.contains("PHASEGENT_REDMINE_REPOSITORY_URL"));
        assert!(text.contains("\"cleared\":true"));
        assert!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_REPOSITORY_URL")
                .unwrap()
                .is_none()
        );
        let outcome2 =
            config_write::clear_setting(None, "PHASEGENT_REDMINE_REPOSITORY_URL", storage).unwrap();
        assert!(
            serde_json::to_string(&outcome2)
                .unwrap()
                .contains("\"cleared\":false")
        );

        let err = config_write::clear_setting(None, "PHASEGENT_API_BASE", storage).unwrap_err();
        assert!(err.contains("a role is required"), "got: {err}");

        config_write::set_setting_value(
            Some(Role::Executor),
            "PHASEGENT_API_BASE",
            "https://a.example",
            storage,
        )
        .unwrap();
        let clear =
            config_write::clear_setting(Some(Role::Executor), "PHASEGENT_API_BASE", storage)
                .unwrap();
        assert!(
            serde_json::to_string(&clear)
                .unwrap()
                .contains("\"cleared\":true")
        );
        assert!(
            storage
                .load_role_config(Role::Executor)
                .unwrap()
                .unwrap()
                .api_base
                .is_none()
        );
        assert!(
            storage
                .load_redmine_config(Role::Executor)
                .unwrap()
                .unwrap()
                .api_base
                .is_none()
        );
        assert!(
            storage
                .load_gitlab_config(Role::Executor)
                .unwrap()
                .unwrap()
                .api_base
                .is_none()
        );
    });
}

#[test]
fn config_clear_command_parsing() {
    let args = ["admin", "config", "clear", "redmine-git-mirror-api-key"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv =
        command::parse_with_role_env(&args, None).expect("clear global without role must parse");
    match inv.command {
        Command::ConfigClear { setting } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        }
        other => panic!("got {other:?}"),
    }
    let args = ["admin", "config", "clear", "api-base"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let err = command::parse_with_role_env(&args, None)
        .expect_err("clear role-scoped without role must error");
    assert!(err.contains("a role is required"), "got: {err}");

    let args = ["admin", "config", "clear", "api-base"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv =
        command::parse_with_role_env(&args, Some("admin")).expect("clear with role must parse");
    match inv.command {
        Command::ConfigClear { setting } => assert_eq!(setting, "PHASEGENT_API_BASE"),
        other => panic!("got {other:?}"),
    }

    let args = ["admin", "config", "clear"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let err = command::parse_with_role_env(&args, Some("executor"))
        .expect_err("clear without setting must error");
    assert!(err.contains("requires a setting"), "got: {err}");

    let args = ["admin", "config", "clear", "unknown"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let err = command::parse_with_role_env(&args, Some("executor"))
        .expect_err("unknown clear setting must error");
    assert!(err.contains("unknown config setting"), "got: {err}");
}
