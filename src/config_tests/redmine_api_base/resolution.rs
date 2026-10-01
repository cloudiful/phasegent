//! Runtime resolution coverage: the env/TOML/SQLite precedence of the global
//! Redmine address, the generic env alias, and explicit overrides.

use super::super::support::*;
use super::super::*;
use super::*;

#[test]
fn global_redmine_api_base_precedence_is_env_over_toml_over_sqlite() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("redmine-global-precedence");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_global_setting("PHASEGENT_REDMINE_API_BASE", "https://sqlite.example")
        .unwrap();
    write_toml_file(&toml_path, "redmine_api_base = \"https://toml.example\"\n");
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(REDMINE_ENV_VARS);

    // TOML shadows the persisted global row.
    assert_eq!(
        auth::redmine_api_base(&storage).unwrap().as_deref(),
        Some("https://toml.example")
    );
    // The Redmine-specific env override shadows TOML.
    let env_redmine = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", "https://env-redmine.example");
    assert_eq!(
        auth::redmine_api_base(&storage).unwrap().as_deref(),
        Some("https://env-redmine.example")
    );
    // The generic env alias sits below the Redmine-specific one.
    let env_generic = EnvGuard::set("PHASEGENT_API_BASE", "https://env-generic.example");
    assert_eq!(
        auth::redmine_api_base(&storage).unwrap().as_deref(),
        Some("https://env-redmine.example")
    );
    drop(env_redmine);
    assert_eq!(
        auth::redmine_api_base(&storage).unwrap().as_deref(),
        Some("https://env-generic.example")
    );
    drop(env_generic);
    assert_eq!(
        auth::redmine_api_base(&storage).unwrap().as_deref(),
        Some("https://toml.example")
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn explicit_api_base_skips_legacy_migration() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("redmine-explicit-skip");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_redmine_config(
            Role::Admin,
            &redmine_config_with_base(Some("https://admin.example")),
        )
        .unwrap();
    storage
        .save_redmine_config(
            Role::Executor,
            &redmine_config_with_base(Some("https://executor.example")),
        )
        .unwrap();
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(REDMINE_ENV_VARS);

    let config = crate::providers::config::RedmineConfig::resolve(
        Role::Executor,
        Some("https://cli.example"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(config.api_base, "https://cli.example");
    assert!(
        storage
            .load_global_setting("PHASEGENT_REDMINE_API_BASE")
            .unwrap()
            .is_none(),
        "an explicit override must not trigger migration"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn generic_api_base_env_is_a_redmine_runtime_alias() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("redmine-generic-alias");
    let storage = Storage::open_at(&db_path).unwrap();
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(REDMINE_ENV_VARS);
    let _generic = EnvGuard::set("PHASEGENT_API_BASE", "https://generic.example");

    assert_eq!(
        auth::redmine_api_base(&storage).unwrap().as_deref(),
        Some("https://generic.example")
    );
    let config =
        crate::providers::config::RedmineConfig::resolve(Role::Executor, None, None, None).unwrap();
    assert_eq!(config.api_base, "https://generic.example");
    // A runtime alias is never persisted.
    assert!(
        storage
            .load_global_setting("PHASEGENT_REDMINE_API_BASE")
            .unwrap()
            .is_none()
    );
    let _ = fs::remove_dir_all(dir);
}
