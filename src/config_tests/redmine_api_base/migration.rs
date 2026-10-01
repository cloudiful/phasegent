//! Bounded legacy role-address migration coverage: single-address migration,
//! TOML shadowing, fail-closed conflicts, and rejection of addresses that are
//! invalid or credential-bearing.

use super::super::support::*;
use super::super::*;
use super::*;

#[test]
fn legacy_single_address_migrates_into_global_setting() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("redmine-migrate-single");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_redmine_config(
            Role::Orchestrator,
            &crate::auth::RedmineStoredConfig {
                api_base: Some("https://legacy.example/".to_owned()),
                close_status_id: Some(5),
                ..Default::default()
            },
        )
        .unwrap();
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(REDMINE_ENV_VARS);

    assert_eq!(
        auth::redmine_api_base(&storage).unwrap().as_deref(),
        Some("https://legacy.example")
    );
    // Migration persists the canonical value into the global row.
    assert_eq!(
        storage
            .load_global_setting("PHASEGENT_REDMINE_API_BASE")
            .unwrap()
            .as_deref(),
        Some("https://legacy.example")
    );
    // Runtime resolution (no explicit base) now reads the global value.
    let config =
        crate::providers::config::RedmineConfig::resolve(Role::Executor, None, None, None).unwrap();
    assert_eq!(config.api_base, "https://legacy.example");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn legacy_role_toml_shadows_sqlite_during_migration() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("redmine-migrate-toml");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_redmine_config(
            Role::Executor,
            &redmine_config_with_base(Some("https://sqlite.example")),
        )
        .unwrap();
    write_toml_file(
        &toml_path,
        "[roles.executor]\nredmine_api_base = \"https://toml.example\"\n",
    );
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(REDMINE_ENV_VARS);

    // The effective legacy value is TOML-over-SQLite, so exactly one
    // address migrates instead of a spurious conflict.
    assert_eq!(
        auth::redmine_api_base(&storage).unwrap().as_deref(),
        Some("https://toml.example")
    );
    assert_eq!(
        storage
            .load_global_setting("PHASEGENT_REDMINE_API_BASE")
            .unwrap()
            .as_deref(),
        Some("https://toml.example")
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn legacy_conflicting_addresses_fail_closed_without_echoing() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("redmine-migrate-conflict");
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

    let error = auth::redmine_api_base(&storage).unwrap_err();
    assert!(error.contains("conflicting"), "got: {error}");
    assert!(error.contains("admin"), "got: {error}");
    assert!(error.contains("executor"), "got: {error}");
    assert!(
        error.contains("redmine-api-base"),
        "error must name the remediation: {error}"
    );
    for leaked in ["admin.example", "executor.example"] {
        assert!(!error.contains(leaked), "error leaked an address: {error}");
    }
    // Fail closed: no global row is written on conflict.
    assert!(
        storage
            .load_global_setting("PHASEGENT_REDMINE_API_BASE")
            .unwrap()
            .is_none()
    );
    // Provider resolution surfaces the same bounded config error.
    let provider_error =
        crate::providers::config::RedmineConfig::resolve(Role::Executor, None, None, None)
            .unwrap_err();
    assert_eq!(provider_error.json()["kind"], "config");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn legacy_migration_rejects_credential_bearing_address_without_persisting() {
    // F2: the bounded migration must reject a credential-bearing legacy
    // address before it can reach the canonical non-sensitive global row.
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("redmine-migrate-creds");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_redmine_config(
            Role::Executor,
            &redmine_config_with_base(Some("http://user:secret@redmine.example")),
        )
        .unwrap();
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(REDMINE_ENV_VARS);

    let error = auth::redmine_api_base(&storage).unwrap_err();
    assert!(error.contains("invalid"), "got: {error}");
    assert!(error.contains("executor"), "got: {error}");
    assert!(
        error.contains("redmine-api-base"),
        "error must name the remediation: {error}"
    );
    for leaked in ["user", "secret", "redmine.example"] {
        assert!(
            !error.contains(leaked),
            "migration error leaked address data '{leaked}': {error}"
        );
    }
    assert!(
        storage
            .load_global_setting("PHASEGENT_REDMINE_API_BASE")
            .unwrap()
            .is_none(),
        "invalid legacy address must not persist to the global row"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn legacy_migration_rejects_invalid_scheme_and_query_without_persisting() {
    let _lock = lock_workflow_tests();
    for (label, bad) in [
        ("scheme", "ftp://redmine.example"),
        ("query", "https://redmine.example?token=hush"),
        ("fragment", "https://redmine.example#frag"),
    ] {
        let (db_path, toml_path, dir) = toml_temp_paths(&format!("redmine-migrate-bad-{label}"));
        let storage = Storage::open_at(&db_path).unwrap();
        storage
            .save_redmine_config(Role::Admin, &redmine_config_with_base(Some(bad)))
            .unwrap();
        let _cfg = EnvGuard::set(
            "PHASEGENT_CONFIG_PATH",
            toml_path.to_string_lossy().as_ref(),
        );
        let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
        let _clear = EnvRemoveGuard::remove(REDMINE_ENV_VARS);
        let error = auth::redmine_api_base(&storage).unwrap_err();
        assert!(error.contains("invalid"), "{label}: got {error}");
        assert!(!error.contains("hush"), "{label}: leaked secret: {error}");
        assert!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_API_BASE")
                .unwrap()
                .is_none(),
            "{label}: invalid address must not persist"
        );
        let _ = fs::remove_dir_all(dir);
    }
}
