//! `admin auth setup --provider redmine` routing coverage plus the credential
//! and identity isolation the global address consolidation must preserve.

use super::super::support::*;
use super::super::*;
use super::*;

#[test]
fn global_address_keeps_role_credentials_and_identity_isolated() {
    let _lock = lock_workflow_tests();
    let (db_path, toml_path, dir) = toml_temp_paths("redmine-cred-isolation");
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .save_credential(Role::Executor, PROVIDER_REDMINE, "executor-key-shhh")
        .unwrap();
    storage
        .save_credential(Role::Reviewer, PROVIDER_REDMINE, "reviewer-key-shhh")
        .unwrap();
    storage
        .save_redmine_user(Role::Executor, 11, "phasegent-executor")
        .unwrap();
    let _cfg = EnvGuard::set(
        "PHASEGENT_CONFIG_PATH",
        toml_path.to_string_lossy().as_ref(),
    );
    let _db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
    let _clear = EnvRemoveGuard::remove(REDMINE_ENV_VARS);

    config_write::set_setting_value(
        None,
        "PHASEGENT_REDMINE_API_BASE",
        "https://redmine.example",
        &storage,
    )
    .unwrap();

    // Consolidating the address never shares or deletes credentials or
    // the provisioned identity.
    assert_eq!(
        storage
            .load_credential(Role::Executor, PROVIDER_REDMINE)
            .unwrap()
            .as_deref(),
        Some("executor-key-shhh")
    );
    assert_eq!(
        storage
            .load_credential(Role::Reviewer, PROVIDER_REDMINE)
            .unwrap()
            .as_deref(),
        Some("reviewer-key-shhh")
    );
    assert_eq!(
        storage.load_redmine_user(Role::Executor).unwrap(),
        Some((11, "phasegent-executor".to_owned()))
    );

    let snapshot = config::show(None, &storage).unwrap();
    let text = serde_json::to_string(&snapshot).unwrap();
    assert!(text.contains("PHASEGENT_REDMINE_API_BASE"));
    for secret in ["executor-key-shhh", "reviewer-key-shhh"] {
        assert!(
            !text.contains(secret),
            "snapshot leaked a credential: {text}"
        );
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn auth_setup_redmine_routes_address_to_the_global_setting() {
    // F1: `admin auth setup --provider redmine --api-base <URL>` must not
    // create a role-scoped legacy address row; the address lands in the
    // canonical global setting while the credential, identity, and
    // close-status stay role-scoped.
    with_isolated_storage("redmine-setup-global", |_db_path, storage| {
        storage
            .save_credential(Role::Executor, PROVIDER_REDMINE, "executor-key-shhh")
            .unwrap();
        storage
            .save_redmine_user(Role::Executor, 22, "phasegent-executor")
            .unwrap();

        crate::auth::save_redmine_config(
            storage,
            Role::Executor,
            Some("https://redmine.example".to_owned()),
            Some("5".to_owned()),
        )
        .unwrap();

        // The address is global, never role-scoped.
        assert_eq!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_API_BASE")
                .unwrap()
                .as_deref(),
            Some("https://redmine.example")
        );
        let role_config = storage
            .load_redmine_config(Role::Executor)
            .unwrap()
            .expect("role row must exist for the close-status id");
        assert_eq!(
            role_config.api_base, None,
            "auth setup must not write a role-scoped Redmine address"
        );
        assert_eq!(role_config.close_status_id, Some(5));
        assert_eq!(
            storage
                .load_role_config(Role::Executor)
                .unwrap()
                .unwrap()
                .provider
                .as_deref(),
            Some(PROVIDER_REDMINE)
        );
        // Credential and identity stay independent and unchanged.
        assert_eq!(
            storage
                .load_credential(Role::Executor, PROVIDER_REDMINE)
                .unwrap()
                .as_deref(),
            Some("executor-key-shhh")
        );
        assert_eq!(
            storage.load_redmine_user(Role::Executor).unwrap(),
            Some((22, "phasegent-executor".to_owned()))
        );
    });
}

#[test]
fn auth_setup_redmine_address_only_writes_no_role_row() {
    // F1: an address-only setup must not create an inert role address row.
    with_isolated_storage("redmine-setup-address-only", |_db_path, storage| {
        crate::auth::save_redmine_config(
            storage,
            Role::Executor,
            Some("https://redmine.example".to_owned()),
            None,
        )
        .unwrap();
        assert_eq!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_API_BASE")
                .unwrap()
                .as_deref(),
            Some("https://redmine.example")
        );
        assert!(
            storage
                .load_redmine_config(Role::Executor)
                .unwrap()
                .and_then(|config| config.api_base)
                .is_none(),
            "address-only setup must not create a role-scoped address"
        );
    });
}

#[test]
fn auth_setup_redmine_rejects_credential_bearing_address() {
    with_isolated_storage("redmine-setup-creds", |_db_path, storage| {
        let error = crate::auth::save_redmine_config(
            storage,
            Role::Executor,
            Some("https://user:secret@redmine.example".to_owned()),
            None,
        )
        .unwrap_err();
        assert!(
            error.contains("must not contain credentials"),
            "got: {error}"
        );
        assert!(
            !error.contains("secret"),
            "error leaked credentials: {error}"
        );
        assert!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_API_BASE")
                .unwrap()
                .is_none(),
            "invalid address must not persist"
        );
    });
}
