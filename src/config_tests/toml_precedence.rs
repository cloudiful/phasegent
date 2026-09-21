use super::support::*;
use super::*;

#[test]
fn toml_missing_file_falls_back_to_sqlite() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("toml-missing");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_REDMINE)
        .unwrap();
    storage
        .save_global_setting(
            "PHASEGENT_REDMINE_REPOSITORY_URL",
            "https://sqlite.example/owner/repo.git",
        )
        .unwrap();
    // Point at a path that does not exist: missing means no overlay.
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(&[
        "PHASEGENT_PROVIDER",
        "PHASEGENT_DEFAULT_PROVIDER",
        "PHASEGENT_REDMINE_REPOSITORY_URL",
    ]);
    assert!(
        crate::infra::config_overlay::load_overlay()
            .unwrap()
            .is_none(),
        "missing TOML must yield no overlay"
    );
    assert_eq!(
        auth::redmine_repository_url_override(&storage)
            .unwrap()
            .as_deref(),
        Some("https://sqlite.example/owner/repo.git")
    );
    let kind = crate::providers::config::resolve_kind(Role::Executor, None).unwrap();
    assert_eq!(kind.as_str(), PROVIDER_REDMINE);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn toml_valid_direct_edits_affect_resolvers() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("toml-valid");
    let storage = Storage::open_at(&db_path).unwrap();
    write_toml_file(
        &toml_path,
        "default_provider = \"gitlab\"\nredmine_repository_url = \"https://toml.example/owner/repo.git\"\nindex_backend = \"sqlite\"\n\n[roles.executor]\nprovider = \"redmine\"\nredmine_api_base = \"https://redmine-toml.example\"\nredmine_close_status_id = 7\nforgejo_api_base = \"https://forgejo-toml.example\"\nforgejo_repository = \"acme/widgets\"\ngitlab_api_base = \"https://gitlab-toml.example\"\n",
    );
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(&[
        "PHASEGENT_PROVIDER",
        "PHASEGENT_DEFAULT_PROVIDER",
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "PHASEGENT_REDMINE_API_BASE",
        "PHASEGENT_API_BASE",
        "PHASEGENT_GITLAB_API_BASE",
        "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
        "PHASEGENT_CLOSE_STATUS_ID",
        "PHASEGENT_REPOSITORY",
    ]);
    // Global overlay affects the same resolver paths as normal commands.
    assert_eq!(
        crate::providers::config::resolve_kind(Role::Executor, None)
            .unwrap()
            .as_str(),
        "gitlab"
    );
    assert_eq!(
        auth::redmine_repository_url_override(&storage)
            .unwrap()
            .as_deref(),
        Some("https://toml.example/owner/repo.git")
    );
    let role = auth::load_config(Role::Executor, &storage)
        .unwrap()
        .expect("TOML-only role row");
    assert_eq!(role.provider.as_deref(), Some("redmine"));
    assert_eq!(
        role.api_base.as_deref(),
        Some("https://forgejo-toml.example")
    );
    assert_eq!(role.repository.as_deref(), Some("acme/widgets"));
    let redmine = auth::load_redmine_config(Role::Executor, &storage)
        .unwrap()
        .expect("TOML redmine row");
    assert_eq!(
        redmine.api_base.as_deref(),
        Some("https://redmine-toml.example")
    );
    assert_eq!(redmine.close_status_id, Some(7));
    let gitlab = auth::load_gitlab_config(Role::Executor, &storage)
        .unwrap()
        .expect("TOML gitlab row");
    assert_eq!(
        gitlab.api_base.as_deref(),
        Some("https://gitlab-toml.example")
    );
    // SQLite stays empty: overlay is read-only, storage is the fallback.
    assert!(
        storage
            .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .unwrap()
            .is_none()
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn toml_precedence_is_env_over_toml_over_sqlite() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("toml-precedence");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_FORGEJO)
        .unwrap();
    storage
        .save_global_setting(
            "PHASEGENT_REDMINE_REPOSITORY_URL",
            "https://sqlite.example/owner/repo.git",
        )
        .unwrap();
    write_toml_file(
        &toml_path,
        "default_provider = \"gitlab\"\nredmine_repository_url = \"https://toml.example/owner/repo.git\"\n",
    );
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(&["PHASEGENT_PROVIDER"]);
    // Env wins over both.
    let _env_default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "redmine");
    let _env_url = EnvGuard::set(
        "PHASEGENT_REDMINE_REPOSITORY_URL",
        "https://env.example/owner/repo.git",
    );
    assert_eq!(
        crate::providers::config::resolve_kind(Role::Executor, None)
            .unwrap()
            .as_str(),
        "redmine"
    );
    assert_eq!(
        auth::redmine_repository_url_override(&storage)
            .unwrap()
            .as_deref(),
        Some("https://env.example/owner/repo.git")
    );
    drop(_env_default);
    drop(_env_url);
    let _clear2 = EnvRemoveGuard::remove(&[
        "PHASEGENT_PROVIDER",
        "PHASEGENT_DEFAULT_PROVIDER",
        "PHASEGENT_REDMINE_REPOSITORY_URL",
    ]);
    // TOML wins over SQLite once env is unset.
    assert_eq!(
        crate::providers::config::resolve_kind(Role::Executor, None)
            .unwrap()
            .as_str(),
        "gitlab"
    );
    assert_eq!(
        auth::redmine_repository_url_override(&storage)
            .unwrap()
            .as_deref(),
        Some("https://toml.example/owner/repo.git")
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn toml_role_precedence_is_env_over_toml_over_sqlite() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("toml-role-precedence");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_role_config(
            Role::Executor,
            &crate::auth::StoredConfig {
                provider: Some(PROVIDER_FORGEJO.to_owned()),
                api_base: Some("https://sqlite-forgejo.example".to_owned()),
                repository: Some("sqlite-owner/sqlite-repo".to_owned()),
            },
        )
        .unwrap();
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
    write_toml_file(
        &toml_path,
        "[roles.executor]\nprovider = \"gitlab\"\nforgejo_api_base = \"https://toml-forgejo.example\"\nforgejo_repository = \"toml-owner/toml-repo\"\nredmine_api_base = \"https://toml-redmine.example\"\nredmine_close_status_id = 9\n",
    );
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(&[
        "PHASEGENT_PROVIDER",
        "PHASEGENT_DEFAULT_PROVIDER",
        "PHASEGENT_API_BASE",
        "PHASEGENT_REPOSITORY",
        "PHASEGENT_REDMINE_API_BASE",
        "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
        "PHASEGENT_CLOSE_STATUS_ID",
    ]);
    // Without env, TOML shadows SQLite.
    let effective = auth::load_config(Role::Executor, &storage)
        .unwrap()
        .unwrap();
    assert_eq!(effective.provider.as_deref(), Some("gitlab"));
    assert_eq!(
        effective.api_base.as_deref(),
        Some("https://toml-forgejo.example")
    );
    assert_eq!(
        effective.repository.as_deref(),
        Some("toml-owner/toml-repo")
    );
    let redmine = auth::load_redmine_config(Role::Executor, &storage)
        .unwrap()
        .unwrap();
    assert_eq!(
        redmine.api_base.as_deref(),
        Some("https://toml-redmine.example")
    );
    assert_eq!(redmine.close_status_id, Some(9));
    // Env still wins over TOML.
    let _env_api = EnvGuard::set("PHASEGENT_API_BASE", "https://env.example");
    let _env_repo = EnvGuard::set("PHASEGENT_REPOSITORY", "env-owner/env-repo");
    let resolved = crate::providers::forgejo::ForgejoConfig::resolve(Role::Executor, None, None);
    // Forgejo resolve prefers env over the TOML-effective stored row.
    // It needs a git origin fallback guard: with env set it must not touch git.
    let forgejo = resolved.expect("env-backed forgejo resolve must succeed");
    assert_eq!(forgejo.base_url, "https://env.example/api/v1");
    assert_eq!(forgejo.owner, "env-owner");
    assert_eq!(forgejo.repository, "env-repo");
    let _ = fs::remove_dir_all(dir);
}
