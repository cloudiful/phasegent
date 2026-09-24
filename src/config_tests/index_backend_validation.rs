use super::support::*;
use super::*;

#[test]
fn index_backend_default_and_postgres_requires_url() {
    with_isolated_storage("index-default-requires-url", |db_path, storage| {
        // `with_isolated_storage` already holds `lock_workflow_tests()`;
        // acquiring it again here would deadlock (non-reentrant mutex).
        let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
        let _unset_backend = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
        let _unset_url = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
        // Default is sqlite when absent.
        let kind = crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            kind,
            crate::infra::issue_index_backend::IndexBackendKind::Sqlite
        );
        // Opening sqlite should succeed (no pg url needed).
        let open = crate::infra::issue_index_backend::IssueIndexBackend::open_blocking_with_storage(
            storage,
        );
        assert!(open.is_ok(), "sqlite open without pg url must succeed");

        // Legacy postgres without URL cannot force Postgres: URL absence
        // selects SQLite and open must succeed (not fail, not fallback).
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "postgres")
            .unwrap();
        storage
            .delete_global_setting("PHASEGENT_INDEX_PG_URL")
            .unwrap();
        let legacy_kind =
            crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            legacy_kind,
            crate::infra::issue_index_backend::IndexBackendKind::Sqlite
        );
        let legacy_open =
            crate::infra::issue_index_backend::IssueIndexBackend::open_blocking_with_storage(
                storage,
            );
        assert!(
            legacy_open.is_ok(),
            "legacy postgres without URL must remain SQLite"
        );

        // Persisted PG URL selects Postgres even when legacy says sqlite;
        // open must attempt Postgres and never silently fallback to SQLite.
        // Use a loopback URL with a closed port so the failure is fast and
        // deterministic with or without the postgres feature (no live DB).
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "sqlite")
            .unwrap();
        storage
            .save_global_setting("PHASEGENT_INDEX_PG_URL", "postgres://127.0.0.1:1/db")
            .unwrap();
        let pg_kind = crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            pg_kind,
            crate::infra::issue_index_backend::IndexBackendKind::Postgres
        );
        let result =
            crate::infra::issue_index_backend::IssueIndexBackend::open_blocking_with_storage(
                storage,
            );
        let err = result
            .err()
            .expect("configured PG URL must not fallback to SQLite");
        assert!(!err.contains("127.0.0.1"), "must not leak url: {err}");
        assert!(!err.contains("postgres://"), "must not leak url: {err}");
    });
}

#[test]
fn config_snapshot_invalid_backend_is_ignored() {
    // Legacy backend is ignored for selection and must never fail
    // `config show`; unknown values surface as absent `value` while
    // presence/length still report the row, without leaking anything.
    with_isolated_storage("snapshot-invalid-backend", |_db_path, storage| {
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "mysql")
            .unwrap();
        let snapshot =
            crate::config::show(None, storage).expect("legacy backend must not fail show");
        let text = serde_json::to_string(&snapshot).unwrap();
        assert!(
            !text.contains("mysql"),
            "snapshot must not echo invalid legacy literal: {text}"
        );
        let entry = snapshot["global_settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "PHASEGENT_INDEX_BACKEND")
            .expect("backend entry");
        assert_eq!(entry["present"], Value::Bool(true));
        assert!(
            entry.get("value").is_none() || entry["value"].is_null(),
            "invalid legacy backend must not expose a value: {entry:?}"
        );
        // Selection still URL-driven: no PG URL means SQLite even with
        // invalid legacy value.
        let _unset_url = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
        let _unset_backend_env = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
        let kind = crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            kind,
            crate::infra::issue_index_backend::IndexBackendKind::Sqlite
        );
    });
}

#[cfg(not(feature = "postgres"))]
#[test]
fn postgres_backend_without_feature_returns_not_enabled() {
    with_isolated_storage("pg-not-enabled", |db_path, storage| {
        let _guard_db = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
        let _unset = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "");
        let _unset_backend_env = EnvGuard::set("PHASEGENT_INDEX_BACKEND", "");
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "postgres")
            .unwrap();
        storage
            .save_global_setting(
                "PHASEGENT_INDEX_PG_URL",
                "postgres://user:pass@localhost/db",
            )
            .unwrap();
        let result =
            crate::infra::issue_index_backend::IssueIndexBackend::open_blocking_with_storage(
                storage,
            );
        let err = result.err().expect("postgres without feature must fail");
        assert!(
            err.contains("postgres index support is not enabled"),
            "got: {err}"
        );
        assert!(!err.contains("pass@localhost"), "must not leak url: {err}");
        // Legacy backend cannot change selection: sqlite + same URL must
        // still attempt Postgres and fail, never fallback to SQLite.
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "sqlite")
            .unwrap();
        let kind = crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            kind,
            crate::infra::issue_index_backend::IndexBackendKind::Postgres
        );
        let result_legacy =
            crate::infra::issue_index_backend::IssueIndexBackend::open_blocking_with_storage(
                storage,
            );
        let err_legacy = result_legacy
            .err()
            .expect("pg url with legacy sqlite must still fail without feature");
        assert!(
            err_legacy.contains("postgres index support is not enabled"),
            "got: {err_legacy}"
        );
        assert!(
            !err_legacy.contains("pass@localhost"),
            "must not leak url: {err_legacy}"
        );
        // Legacy postgres without URL cannot force Postgres: must be SQLite
        // and open must succeed.
        storage
            .delete_global_setting("PHASEGENT_INDEX_PG_URL")
            .unwrap();
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "postgres")
            .unwrap();
        let kind_sqlite =
            crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            kind_sqlite,
            crate::infra::issue_index_backend::IndexBackendKind::Sqlite
        );
        let open_sqlite =
            crate::infra::issue_index_backend::IssueIndexBackend::open_blocking_with_storage(
                storage,
            );
        assert!(
            open_sqlite.is_ok(),
            "legacy postgres without URL must remain SQLite"
        );
        // Also via env: env URL selects Postgres even when persisted is
        // absent and legacy says sqlite, and must fail without fallback.
        storage
            .save_global_setting("PHASEGENT_INDEX_BACKEND", "sqlite")
            .unwrap();
        let _guard_url = EnvGuard::set("PHASEGENT_INDEX_PG_URL", "postgres://user:secret@host/db");
        let kind_env = crate::infra::issue_index_backend::resolve_index_backend(storage).unwrap();
        assert_eq!(
            kind_env,
            crate::infra::issue_index_backend::IndexBackendKind::Postgres
        );
        let result2 =
            crate::infra::issue_index_backend::IssueIndexBackend::open_blocking_with_storage(
                storage,
            );
        let err2 = result2
            .err()
            .expect("postgres env without feature must fail");
        assert!(
            err2.contains("postgres index support is not enabled"),
            "got: {err2}"
        );
        assert!(!err2.contains("secret"), "must not leak url: {err2}");
    });
}

#[test]
fn removed_index_commands_and_help_topics_are_rejected() {
    // `issue index sync/search` were removed; ordinary `issue search` is the
    // only documented workflow with automatic warm/fallback. The removed
    // command surface must fail at the CLI and under `--help`, and the
    // overview must not advertise a topic whose name starts with `index`.
    for args in [
        vec!["issue", "index", "sync", "--query", "bug"],
        vec!["issue", "index", "search", "--query", "hello"],
        vec!["issue", "index"],
    ] {
        let parsed: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let error = command::parse_with_role_env(&parsed, Some("executor"))
            .expect_err("removed index command must be rejected");
        assert!(
            error.contains("unknown issue command") && error.contains("index"),
            "got: {error}"
        );
    }
    for topic in ["index", "index sync", "index search"] {
        let mut parts = vec!["--help".to_owned(), "issue".to_owned()];
        parts.extend(topic.split_whitespace().map(str::to_owned));
        let error = command::parse_with_role_env(&parts, Some("executor"))
            .err()
            .unwrap_or_else(|| panic!("help {topic} must be rejected"));
        assert!(
            error.contains("unknown issue help topic"),
            "help {topic} must be rejected as unknown help topic, got: {error}"
        );
    }
    // Ordinary search still parses with the transparent flags.
    let args = ["issue", "search", "--query", "phase"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation =
        command::parse_with_role_env(&args, Some("executor")).expect("ordinary search must parse");
    match invocation.command {
        Command::Issue(crate::command::IssueCommand::Search { query, .. }) => {
            assert_eq!(query.as_deref(), Some("phase"));
        }
        other => panic!("expected Search, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// TOML overlay: read-only human-editable layer with
// explicit CLI > env > TOML > SQLite > defaults precedence.
// ---------------------------------------------------------------------------
