use super::support::*;
use super::*;

#[test]
fn toml_malformed_and_unknown_fields_fail_clearly() {
    let _lock = lock_workflow_tests();
    let err = crate::infra::config_overlay::parse_overlay_str(
        "default_provider = [unclosed\n",
        "test.toml",
    )
    .unwrap_err();
    assert!(err.contains("could not parse TOML"), "got: {err}");
    assert!(err.contains("test.toml"), "got: {err}");
    let err = crate::infra::config_overlay::parse_overlay_str("unknown_key = \"x\"\n", "test.toml")
        .unwrap_err();
    assert!(
        err.contains("unknown") || err.contains("could not parse"),
        "got: {err}"
    );
    let err = crate::infra::config_overlay::parse_overlay_str(
        "[roles.executor]\nunknown_role_field = \"x\"\n",
        "test.toml",
    )
    .unwrap_err();
    assert!(
        err.contains("unknown") || err.contains("could not parse"),
        "got: {err}"
    );
    let err = crate::infra::config_overlay::parse_overlay_str(
        "[roles.unknown]\nprovider = \"redmine\"\n",
        "test.toml",
    )
    .unwrap_err();
    assert!(err.contains("unknown role"), "got: {err}");
    let err = crate::infra::config_overlay::parse_overlay_str(
        "default_provider = \"wrong\"\n",
        "test.toml",
    )
    .unwrap_err();
    assert!(err.contains("invalid default_provider"), "got: {err}");
    assert!(err.contains("wrong"), "got: {err}");
}

#[test]
fn toml_secret_and_runtime_fields_are_rejected_without_echo() {
    let _lock = lock_workflow_tests();
    for (label, content, secret) in [
        (
            "pg-url",
            "index_pg_url = \"postgres://user:pass@localhost/db\"\n",
            "postgres://user:pass@localhost/db",
        ),
        (
            "mirror-key",
            "redmine_git_mirror_api_key = \"mirror-bearer-shhh\"\n",
            "mirror-bearer-shhh",
        ),
        (
            "token",
            "[roles.executor]\ntoken = \"forgejo-shhh\"\n",
            "forgejo-shhh",
        ),
        (
            "project-id",
            "[roles.executor]\nredmine_api_base = \"https://redmine.example\"\nproject_id = \"42\"\n",
            "42",
        ),
        (
            "redmine-url-creds",
            "redmine_repository_url = \"https://user:s3cret@host.example/owner/repo.git\"\n",
            "s3cret",
        ),
        (
            "api-base-creds",
            "[roles.executor]\nforgejo_api_base = \"https://user:s3cret@host.example\"\n",
            "s3cret",
        ),
    ] {
        let err =
            crate::infra::config_overlay::parse_overlay_str(content, "test.toml").unwrap_err();
        assert!(
            err.contains("rejects secret") || err.contains("must not contain credentials"),
            "{label} must fail as secret: {err}"
        );
        assert!(
            !err.contains(secret),
            "{label} error leaked secret '{secret}': {err}"
        );
    }
}

#[test]
fn toml_path_isolation_requires_absolute() {
    let _lock = lock_workflow_tests();
    let (_db_path, toml_path, dir) = toml_temp_paths("toml-path");
    write_toml_file(&toml_path, "default_provider = \"redmine\"\n");
    let _relative = EnvGuard::set("PHASEGENT_CONFIG_PATH", "relative/phasegent.toml");
    let err = crate::infra::config_overlay::load_overlay().unwrap_err();
    assert!(err.contains("absolute"), "got: {err}");
    assert!(
        !err.contains("redmine"),
        "path error must not echo file values"
    );
    drop(_relative);
    // Absolute override is honoured.
    let _absolute = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let overlay = crate::infra::config_overlay::load_overlay()
        .unwrap()
        .expect("absolute path must load");
    assert_eq!(overlay.default_provider_value(), Some("redmine"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn toml_overlay_is_read_only_for_set_and_clear() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("toml-readonly");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_FORGEJO)
        .unwrap();
    write_toml_file(&toml_path, "default_provider = \"gitlab\"\n");
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(&["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"]);
    // Effective is TOML even though SQLite holds a different value.
    assert_eq!(
        crate::providers::config::resolve_kind(Role::Executor, None)
            .unwrap()
            .as_str(),
        "gitlab"
    );
    // `config set` still writes SQLite only (read-only overlay contract).
    crate::config_write::set_setting_value(None, "PHASEGENT_DEFAULT_PROVIDER", "redmine", &storage)
        .unwrap();
    assert_eq!(
        storage
            .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .unwrap()
            .as_deref(),
        Some("redmine")
    );
    assert_eq!(
        crate::providers::config::resolve_kind(Role::Executor, None)
            .unwrap()
            .as_str(),
        "gitlab",
        "TOML must still shadow SQLite after a SQLite write"
    );
    // `config clear` removes the SQLite row but TOML still shadows.
    crate::config_write::clear_setting(None, "PHASEGENT_DEFAULT_PROVIDER", &storage).unwrap();
    assert!(
        storage
            .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        crate::providers::config::resolve_kind(Role::Executor, None)
            .unwrap()
            .as_str(),
        "gitlab",
        "clearing SQLite must not clear TOML"
    );
    // SQLite snapshot stays persisted-view and redacted/backward-compatible.
    let snapshot = config::show(None, &storage).unwrap();
    assert!(
        snapshot["global_default_provider"].is_null(),
        "persisted snapshot must reflect SQLite, not TOML: {snapshot:?}"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn toml_index_backend_validated_but_ignored_for_selection() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("toml-index-backend");
    let storage = Storage::open_at(&db_path).unwrap();
    write_toml_file(&toml_path, "index_backend = \"postgres\"\n");
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _pg = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
    // Legacy backend is validated...
    let overlay = crate::infra::config_overlay::load_overlay()
        .unwrap()
        .expect("overlay");
    assert_eq!(overlay.index_backend_value(), Some("postgres"));
    // ...but selection remains URL-driven: absent URL means SQLite.
    assert_eq!(
        crate::infra::issue_index_backend::resolve_index_backend(&storage).unwrap(),
        crate::infra::issue_index_backend::IndexBackendKind::Sqlite
    );
    // Invalid backend fails fast instead of being ignored.
    let err =
        crate::infra::config_overlay::parse_overlay_str("index_backend = \"mysql\"\n", "test.toml")
            .unwrap_err();
    assert!(err.contains("invalid index_backend"), "got: {err}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn toml_redmine_and_gitlab_resolvers_prefer_toml_over_sqlite() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("toml-resolvers");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_redmine_config(
            Role::Executor,
            &crate::auth::RedmineStoredConfig {
                api_base: Some("https://sqlite-redmine.example".to_owned()),
                project_id: None,
                close_status_id: Some(5),
                group_name: None,
                group_role: None,
            },
        )
        .unwrap();
    storage
        .save_gitlab_config(
            Role::Executor,
            &crate::auth::GitlabStoredConfig {
                api_base: Some("https://sqlite-gitlab.example".to_owned()),
                project_id: None,
            },
        )
        .unwrap();
    write_toml_file(
        &toml_path,
        "[roles.executor]\nredmine_api_base = \"https://toml-redmine.example\"\nredmine_close_status_id = 11\ngitlab_api_base = \"https://toml-gitlab.example\"\n",
    );
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(&[
        "PHASEGENT_REDMINE_API_BASE",
        "PHASEGENT_API_BASE",
        "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
        "PHASEGENT_CLOSE_STATUS_ID",
        "PHASEGENT_GITLAB_API_BASE",
    ]);
    let redmine =
        crate::providers::config::RedmineConfig::resolve(Role::Executor, None, None, None).unwrap();
    assert_eq!(redmine.api_base, "https://toml-redmine.example");
    assert_eq!(redmine.close_status_id, Some(11));
    let gitlab =
        crate::providers::config::GitlabConfig::resolve(Role::Executor, None, Some("77")).unwrap();
    assert_eq!(gitlab.api_base, "https://toml-gitlab.example/api/v4");
    // Explicit CLI still wins over TOML.
    let explicit = crate::providers::config::RedmineConfig::resolve(
        Role::Executor,
        Some("https://cli.example"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(explicit.api_base, "https://cli.example");
    let _ = fs::remove_dir_all(dir);
    let _ = storage;
}
