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
            "PHASEGENT_API_BASE",
            "https://forgejo.example",
            storage,
        )
        .unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        assert!(text.contains("PHASEGENT_API_BASE"));
        assert!(
            !text.contains("https://forgejo.example"),
            "value must not be echoed: {text}"
        );
        // Verify storage: the generic api-base writes the Forgejo and
        // GitLab role rows only. It must not create or update the legacy
        // role-scoped Redmine address row (the canonical Redmine address
        // is the global `PHASEGENT_REDMINE_API_BASE` setting).
        let forgejo = storage.load_role_config(Role::Executor).unwrap().unwrap();
        assert_eq!(forgejo.api_base.as_deref(), Some("https://forgejo.example"));
        let gitlab = storage.load_gitlab_config(Role::Executor).unwrap().unwrap();
        assert_eq!(gitlab.api_base.as_deref(), Some("https://forgejo.example"));
        assert!(
            storage
                .load_redmine_config(Role::Executor)
                .unwrap()
                .and_then(|config| config.api_base)
                .is_none(),
            "generic api-base must not write the Redmine role row"
        );
        assert!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_API_BASE")
                .unwrap()
                .is_none(),
            "generic api-base must not write the global Redmine address"
        );

        // Project-id aliases are now rejected; verify they do not persist.
        assert!(config_write::canonical_setting_name("redmine-project-id").is_none());
        assert!(config_write::canonical_setting_name("gitlab-project-id").is_none());
        assert!(config_write::canonical_setting_name("project-id").is_none());
    });
}

#[test]
fn config_set_default_provider_reuses_validation() {
    with_isolated_storage("set-default-provider", |_db_path, storage| {
        for literal in [PROVIDER_FORGEJO, PROVIDER_REDMINE, PROVIDER_GITLAB] {
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
                .load_gitlab_config(Role::Executor)
                .unwrap()
                .unwrap()
                .api_base
                .is_none()
        );
        // The generic alias never touches the legacy Redmine role row or
        // the global Redmine address.
        assert!(
            storage
                .load_redmine_config(Role::Executor)
                .unwrap()
                .and_then(|config| config.api_base)
                .is_none()
        );
        assert!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_API_BASE")
                .unwrap()
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
