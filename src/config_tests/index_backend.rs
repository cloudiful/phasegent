use super::support::*;
use super::*;

#[test]
fn index_backend_alias_and_validation() {
    // Canonical and kebab aliases must resolve, case-insensitive, and validation must accept only sqlite/postgres.
    for alias in [
        "PHASEGENT_INDEX_BACKEND",
        "index-backend",
        "INDEX_BACKEND",
        "index_backend",
        "PHASEGENT_index_backend",
    ] {
        let canonical = crate::config_write::canonical_setting_name(alias)
            .unwrap_or_else(|| panic!("alias {alias} must resolve"));
        assert_eq!(canonical, "PHASEGENT_INDEX_BACKEND");
    }
    for alias in [
        "PHASEGENT_INDEX_PG_URL",
        "index-pg-url",
        "INDEX_PG_URL",
        "index_pg_url",
    ] {
        let canonical = crate::config_write::canonical_setting_name(alias)
            .unwrap_or_else(|| panic!("alias {alias} must resolve"));
        assert_eq!(canonical, "PHASEGENT_INDEX_PG_URL");
    }
    assert!(crate::config_write::is_global_setting(
        "PHASEGENT_INDEX_BACKEND"
    ));
    assert!(crate::config_write::is_global_setting(
        "PHASEGENT_INDEX_PG_URL"
    ));
    assert!(!crate::config_write::is_secret_setting(
        "PHASEGENT_INDEX_BACKEND"
    ));
    assert!(crate::config_write::is_secret_setting(
        "PHASEGENT_INDEX_PG_URL"
    ));

    // Parser: backend without role, pg-url requires stdin/prompt and rejects direct value.
    let args = ["admin", "config", "set", "index-backend", "postgres"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv = command::parse(&args).expect("index-backend set without role must parse");
    match inv.command {
        Command::ConfigSet { setting, .. } => assert_eq!(setting, "PHASEGENT_INDEX_BACKEND"),
        other => panic!("unexpected {other:?}"),
    }
    let args = ["admin", "config", "set", "index-pg-url", "--stdin"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv = command::parse(&args).expect("index-pg-url --stdin without role must parse");
    match inv.command {
        Command::ConfigSet { setting, stdin, .. } => {
            assert_eq!(setting, "PHASEGENT_INDEX_PG_URL");
            assert!(stdin);
        }
        other => panic!("unexpected {other:?}"),
    }
    let args = [
        "admin",
        "config",
        "set",
        "index-pg-url",
        "postgres://example",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let err = command::parse(&args).expect_err("direct secret value must be rejected");
    assert!(err.contains("does not accept a direct value"), "got: {err}");
    assert!(
        !err.contains("postgres://example"),
        "error must not echo secret"
    );

    // Dispatch validation: backend rejects unknown literals, trims and lowercases.
    with_isolated_storage("index-backend-validation", |_db_path, storage| {
        for bad in ["", "   ", "mysql", "sqlite2", "postgres!"] {
            let trimmed = bad.trim();
            if trimmed.is_empty() {
                let err = crate::config_write::set_setting_value(
                    None,
                    "PHASEGENT_INDEX_BACKEND",
                    bad,
                    storage,
                )
                .unwrap_err();
                assert!(err.contains("cannot be empty"), "got: {err}");
            } else {
                let err = crate::config_write::set_setting_value(
                    None,
                    "PHASEGENT_INDEX_BACKEND",
                    bad,
                    storage,
                )
                .unwrap_err();
                assert!(
                    err.contains("invalid PHASEGENT_INDEX_BACKEND"),
                    "got: {err}"
                );
                assert!(!err.contains("secret"), "no secret leak: {err}");
            }
        }
        // Valid values persist lowercased.
        crate::config_write::set_setting_value(
            None,
            "PHASEGENT_INDEX_BACKEND",
            "POSTGRES",
            storage,
        )
        .unwrap();
        assert_eq!(
            storage
                .load_global_setting("PHASEGENT_INDEX_BACKEND")
                .unwrap()
                .as_deref(),
            Some("postgres")
        );
        crate::config_write::set_setting_value(
            None,
            "PHASEGENT_INDEX_BACKEND",
            "  Sqlite  ",
            storage,
        )
        .unwrap();
        assert_eq!(
            storage
                .load_global_setting("PHASEGENT_INDEX_BACKEND")
                .unwrap()
                .as_deref(),
            Some("sqlite")
        );
    });
}

#[test]
fn index_pg_url_secret_via_stdin_and_snapshot_redacted() {
    with_isolated_storage("index-pg-url-stdin", |_db_path, storage| {
        let secret = "postgres://user:pass@localhost/indexdb";
        // Secret must be via stdin helper, not direct set_setting_value.
        let err =
            crate::config_write::set_setting_value(None, "PHASEGENT_INDEX_PG_URL", secret, storage)
                .unwrap_err();
        assert!(err.contains("secret setting"), "got: {err}");
        assert!(!err.contains(secret), "error must not echo secret");

        let outcome = crate::config_write::set_setting_stdin_content(
            None,
            "PHASEGENT_INDEX_PG_URL",
            &format!("  {secret}  \n"),
            storage,
        )
        .unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        assert!(text.contains("PHASEGENT_INDEX_PG_URL"));
        assert!(
            !text.contains(secret),
            "outcome must not echo secret: {text}"
        );

        let stored = storage
            .load_global_setting("PHASEGENT_INDEX_PG_URL")
            .unwrap()
            .expect("stored");
        assert_eq!(stored, secret);

        // Snapshot must redact.
        let snapshot = crate::config::show(None, storage).unwrap();
        let snap_text = serde_json::to_string(&snapshot).unwrap();
        assert!(
            !snap_text.contains(secret),
            "snapshot leaked secret: {snap_text}"
        );
        assert!(
            !snap_text.contains("pass@localhost"),
            "snapshot leaked: {snap_text}"
        );
        let global = snapshot["global_settings"].as_array().unwrap();
        let entry = global
            .iter()
            .find(|e| e["name"] == "PHASEGENT_INDEX_PG_URL")
            .unwrap();
        assert_eq!(entry["present"], Value::Bool(true));
        assert_eq!(entry["length"], Value::from(secret.chars().count()));
        assert!(
            entry.get("value").is_none(),
            "secret must not expose value: {entry:?}"
        );
        assert!(
            entry.get("sanitized_value").is_none(),
            "secret must not expose sanitized: {entry:?}"
        );

        // Backend is non-secret literal and should appear as value.
        crate::config_write::set_setting_value(
            None,
            "PHASEGENT_INDEX_BACKEND",
            "postgres",
            storage,
        )
        .unwrap();
        let snapshot2 = crate::config::show(None, storage).unwrap();
        let global2 = snapshot2["global_settings"].as_array().unwrap();
        let backend = global2
            .iter()
            .find(|e| e["name"] == "PHASEGENT_INDEX_BACKEND")
            .unwrap();
        assert_eq!(backend["present"], Value::Bool(true));
        assert_eq!(backend["value"], Value::String("postgres".to_owned()));
        let snap2_text = serde_json::to_string(&snapshot2).unwrap();
        assert!(
            snap2_text.contains("postgres"),
            "backend literal should be visible: {snap2_text}"
        );
        assert!(!snap2_text.contains(secret), "still redacted: {snap2_text}");
    });
}

#[test]
fn index_backend_clear_without_role_and_env_precedence() {
    with_isolated_storage("index-clear", |db_path, storage| {
        // Global clear without role.
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "postgres")
            .unwrap();
        storage
            .save_global_setting("PHASEGENT_INDEX_PG_URL", "postgres://example/db")
            .unwrap();
        let out =
            crate::config_write::clear_setting(None, "PHASEGENT_INDEX_BACKEND", storage).unwrap();
        assert!(
            serde_json::to_string(&out)
                .unwrap()
                .contains("\"cleared\":true")
        );
        assert!(
            storage
                .load_global_setting("PHASEGENT_INDEX_BACKEND")
                .unwrap()
                .is_none()
        );
        let out2 =
            crate::config_write::clear_setting(None, "PHASEGENT_INDEX_PG_URL", storage).unwrap();
        assert!(
            serde_json::to_string(&out2)
                .unwrap()
                .contains("\"cleared\":true")
        );
        assert!(
            storage
                .load_global_setting("PHASEGENT_INDEX_PG_URL")
                .unwrap()
                .is_none()
        );
        // Second clear is false.
        let out3 =
            crate::config_write::clear_setting(None, "PHASEGENT_INDEX_BACKEND", storage).unwrap();
        assert!(
            serde_json::to_string(&out3)
                .unwrap()
                .contains("\"cleared\":false")
        );

        // URL-driven selection: persisted PG URL selects Postgres even when
        // legacy backend says sqlite; legacy env/persisted can never force a
        // different backend. Env PG URL overrides persisted URL and backend.
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "sqlite")
            .unwrap();
        storage
            .save_global_setting("PHASEGENT_INDEX_PG_URL", "postgres://stored/db")
            .unwrap();
        let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
        // Legacy backend env must be ignored: even postgres cannot change
        // selection without a URL, and sqlite cannot block a URL.
        let _guard_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "postgres");
        let _guard_url = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "postgres://env/db");
        // Resolve via backend helper (local config only, no provider).
        let kind = crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            kind,
            crate::infra::issue_index_backend::IndexBackendKind::Postgres
        );
        let url = crate::infra::issue_index_backend::resolve_pg_url(storage).unwrap();
        assert_eq!(url.as_deref(), Some("postgres://env/db"));

        // Unset env -> fallback to persisted URL (blank env is unset).
        // Persisted URL still selects Postgres even though legacy says sqlite.
        drop(_guard_backend);
        drop(_guard_url);
        let _unset_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
        let _unset_url = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
        let kind2 = crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            kind2,
            crate::infra::issue_index_backend::IndexBackendKind::Postgres
        );
        let url2 = crate::infra::issue_index_backend::resolve_pg_url(storage).unwrap();
        assert_eq!(url2.as_deref(), Some("postgres://stored/db"));

        // No PG URL selects SQLite even if legacy backend says postgres.
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "postgres")
            .unwrap();
        storage
            .delete_global_setting("PHASEGENT_INDEX_PG_URL")
            .unwrap();
        let kind3 = crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            kind3,
            crate::infra::issue_index_backend::IndexBackendKind::Sqlite
        );
        let url3 = crate::infra::issue_index_backend::resolve_pg_url(storage).unwrap();
        assert_eq!(url3, None);

        // Blank env URL selects SQLite when nothing is persisted.
        let _blank_url = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "   ");
        let kind4 = crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            kind4,
            crate::infra::issue_index_backend::IndexBackendKind::Sqlite
        );
    });
}
