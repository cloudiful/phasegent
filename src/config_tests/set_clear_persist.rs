use super::support::*;
use super::*;

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
            "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
            "6",
            storage,
        )
        .unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        assert!(text.contains("PHASEGENT_REDMINE_CLOSE_STATUS_ID"));
        assert!(!text.contains("6"), "value must not be echoed: {text}");
        let stored = storage
            .load_redmine_config(Role::Executor)
            .unwrap()
            .and_then(|config| config.close_status_id);
        assert_eq!(stored, Some(6));

        // Retired-provider settings and the removed generic aliases no
        // longer resolve, so they are rejected as unknown settings.
        for removed in [
            "api-base",
            "repository",
            "gitlab-api-base",
            "redmine-project-id",
            "gitlab-project-id",
            "project-id",
        ] {
            assert!(
                config_write::canonical_setting_name(removed).is_none(),
                "{removed} must no longer resolve"
            );
        }
    });
}

#[test]
fn config_set_default_provider_reuses_validation() {
    with_isolated_storage("set-default-provider", |_db_path, storage| {
        for literal in [PROVIDER_REDMINE, "local"] {
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

        let err = config_write::clear_setting(None, "PHASEGENT_REDMINE_CLOSE_STATUS_ID", storage)
            .unwrap_err();
        assert!(err.contains("a role is required"), "got: {err}");

        config_write::set_setting_value(
            Some(Role::Executor),
            "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
            "6",
            storage,
        )
        .unwrap();
        let clear = config_write::clear_setting(
            Some(Role::Executor),
            "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
            storage,
        )
        .unwrap();
        assert!(
            serde_json::to_string(&clear)
                .unwrap()
                .contains("\"cleared\":true")
        );
        assert!(
            storage
                .load_redmine_config(Role::Executor)
                .unwrap()
                .and_then(|config| config.close_status_id)
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
    let args = ["admin", "config", "clear", "redmine-close-status-id"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let err = command::parse_with_role_env(&args, None)
        .expect_err("clear role-scoped without role must error");
    assert!(err.contains("a role is required"), "got: {err}");

    let args = ["admin", "config", "clear", "redmine-close-status-id"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv =
        command::parse_with_role_env(&args, Some("admin")).expect("clear with role must parse");
    match inv.command {
        Command::ConfigClear { setting } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        }
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
