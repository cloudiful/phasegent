//! Focused tests for the `config show`, `config set`/`clear`, and
//! `config provider` flows.
//!
//! These tests cover:
//! * `config show` redacts credentials
//! * `config set`/`clear` parser acceptance/rejection, alias handling,
//!   secret handling via `--stdin` / interactive path, and persistence
//! * `config import-env` removal
//! * `project list` does not require a project id
//! * provider get/set/clear and snapshot reporting
//!
//! Tests build a fresh `Storage` via [`Storage::open_at`] against a
//! private temp database.

use crate::auth;
use crate::command::{self, Command, ProjectCommand};
use crate::config;
use crate::config_write;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{
    DB_FILENAME, PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE, Storage,
};
use crate::policy::Role;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn unique_temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "phasegent-config-{label}-{}-{}",
        std::process::id(),
        system_time_nanos()
    ))
}

fn system_time_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn unique_temp_db_path(label: &str) -> PathBuf {
    unique_temp_dir(label).join(DB_FILENAME)
}

fn with_isolated_storage<T>(label: &str, f: impl FnOnce(&Path, &Storage) -> T) -> T {
    let _lock = lock_workflow_tests();
    let db_path = unique_temp_db_path(label);
    let storage = Storage::open_at(&db_path).unwrap();
    let result = f(&db_path, &storage);
    let _ = fs::remove_dir_all(db_path.parent().unwrap());
    result
}

#[test]
fn config_show_redacts_credentials_and_sanitises_url() {
    with_isolated_storage("show-redact", |_db_path, storage| {
        storage
            .save_credential(Role::Executor, PROVIDER_FORGEJO, "forgejo-secret-token")
            .unwrap();
        storage
            .save_credential(Role::Executor, PROVIDER_REDMINE, "redmine-secret-key")
            .unwrap();
        storage
            .save_global_setting(
                "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
                "mirror-bearer-key-shhh",
            )
            .unwrap();
        storage
            .save_global_setting(
                "PHASEGENT_REDMINE_REPOSITORY_URL",
                "https://user:password@hush.example.com/owner/repo.git?token=hush#fragment",
            )
            .unwrap();

        let snapshot = config::show(Some(Role::Executor), storage).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();

        for forbidden in [
            "forgejo-secret-token",
            "redmine-secret-key",
            "mirror-bearer-key-shhh",
            "password",
        ] {
            assert!(
                !text.contains(forbidden),
                "snapshot leaked '{forbidden}': {text}"
            );
        }

        assert!(text.contains("hush.example.com"));
        assert!(!text.contains("?token="));
        assert!(!text.contains("#fragment"));
        assert!(!text.contains("user:"));

        let roles = snapshot["roles"].as_array().expect("roles array");
        assert_eq!(roles.len(), 1);
        let executor = &roles[0];
        assert_eq!(executor["role"], "executor");
        assert_eq!(executor["forgejo_credential"]["present"], Value::Bool(true));
        assert_eq!(
            executor["forgejo_credential"]["length"],
            Value::from("forgejo-secret-token".len())
        );
        assert_eq!(executor["redmine_credential"]["present"], Value::Bool(true));
        assert_eq!(
            executor["redmine_credential"]["length"],
            Value::from("redmine-secret-key".len())
        );
        let global = snapshot["global_settings"].as_array().expect("global");
        assert_eq!(global.len(), 3);
        let key = global
            .iter()
            .find(|entry| entry["name"] == "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
            .expect("mirror key entry");
        assert_eq!(key["present"], Value::Bool(true));
        assert_eq!(key["length"], Value::from("mirror-bearer-key-shhh".len()));
        let url = global
            .iter()
            .find(|entry| entry["name"] == "PHASEGENT_REDMINE_REPOSITORY_URL")
            .expect("mirror url entry");
        assert_eq!(url["present"], Value::Bool(true));
        assert!(
            url["sanitized_value"]
                .as_str()
                .unwrap()
                .contains("hush.example.com"),
            "sanitized URL must keep the host: {url:?}"
        );
    });
}

#[test]
fn config_show_replaces_unparseable_mirror_url_with_safe_placeholder() {
    with_isolated_storage("show-bad-url-redact", |_db_path, storage| {
        let malicious_inputs = [
            "git@user:password@host.example.com:owner/repo.git",
            "https://user:pa$$word@example.com:owner/repo.git",
            "https://user:password@example.com:notaport/path",
        ];
        storage
            .save_global_setting("PHASEGENT_REDMINE_REPOSITORY_URL", malicious_inputs[0])
            .unwrap();

        let snapshot = config::show(None, storage).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();

        let url_entry = snapshot["global_settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "PHASEGENT_REDMINE_REPOSITORY_URL")
            .expect("mirror url entry");
        assert_eq!(
            url_entry["sanitized_value"].as_str(),
            Some(crate::config_snapshot::INVALID_URL_PLACEHOLDER),
            "unparseable URL must surface as the placeholder"
        );
        for forbidden in malicious_inputs {
            for fragment in forbidden.split([':', '@', '/', '?', '#', ' ']) {
                if fragment.is_empty() {
                    continue;
                }
                if matches!(
                    fragment,
                    "https"
                        | "http"
                        | "ssh"
                        | "git"
                        | "example.com"
                        | "git.example.com"
                        | "host.example.com"
                        | "owner"
                        | "repo.git"
                        | "path"
                ) {
                    continue;
                }
                assert!(
                    !text.contains(fragment),
                    "snapshot leaked forbidden fragment '{fragment}' from input '{forbidden}': {text}"
                );
            }
        }
    });
}

#[test]
fn config_show_without_role_reports_every_role() {
    with_isolated_storage("show-global", |_db_path, storage| {
        for role in [
            Role::Admin,
            Role::Orchestrator,
            Role::Executor,
            Role::Reviewer,
            Role::Tester,
        ] {
            let config = crate::auth::StoredConfig {
                provider: Some(PROVIDER_FORGEJO.to_owned()),
                ..Default::default()
            };
            storage.save_role_config(role, &config).unwrap();
        }

        let snapshot = config::show(None, storage).unwrap();
        let roles = snapshot["roles"].as_array().expect("roles array");
        let names: Vec<&str> = roles
            .iter()
            .map(|entry| entry["role"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            vec!["admin", "orchestrator", "executor", "reviewer", "tester"],
            "global config show must enumerate every known role"
        );
        assert!(
            snapshot["database_path"]
                .as_str()
                .unwrap_or_default()
                .ends_with("phasegent.sqlite3"),
            "snapshot must name the SQLite database path: {snapshot:?}"
        );
    });
}

#[test]
fn ordinary_provider_commands_do_not_persist_env_values() {
    with_isolated_storage("no-implicit-persist", |_db_path, storage| {
        let _provider = EnvGuard::set("PHASEGENT_PROVIDER", "redmine");
        let resolved = crate::providers::config::resolve_kind(Role::Executor, None).unwrap();
        assert_eq!(resolved.as_str(), "redmine");
        let role_config = storage.load_role_config(Role::Executor).unwrap();
        assert!(
            role_config.is_none(),
            "ordinary command must not write a provider row: {role_config:?}"
        );
        assert!(
            storage
                .load_global_setting("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
                .unwrap()
                .is_none(),
            "mirror bearer key must not leak into SQLite on an ordinary run"
        );
    });
}

#[test]
fn mirror_fallback_prefers_environment_then_sqlite() {
    with_isolated_storage("mirror-fallback", |_db_path, storage| {
        storage
            .save_global_setting("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "sqlite-bearer-key")
            .unwrap();
        storage
            .save_global_setting(
                "PHASEGENT_REDMINE_REPOSITORY_URL",
                "https://sqlite.example/owner/repo.git",
            )
            .unwrap();

        let _unset_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "");
        let _unset_url = EnvGuard::set("PHASEGENT_REDMINE_REPOSITORY_URL", "");
        assert_eq!(
            auth::redmine_git_mirror_api_key(storage).unwrap(),
            Some("sqlite-bearer-key".to_owned())
        );
        assert_eq!(
            auth::redmine_repository_url_override(storage).unwrap(),
            Some("https://sqlite.example/owner/repo.git".to_owned())
        );

        let _env_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "env-bearer-key");
        let _env_url = EnvGuard::set(
            "PHASEGENT_REDMINE_REPOSITORY_URL",
            "https://env.example/owner/repo.git",
        );
        assert_eq!(
            auth::redmine_git_mirror_api_key(storage).unwrap(),
            Some("env-bearer-key".to_owned())
        );
        assert_eq!(
            auth::redmine_repository_url_override(storage).unwrap(),
            Some("https://env.example/owner/repo.git".to_owned())
        );
    });
}

#[test]
fn mirror_fallback_returns_none_when_no_source_is_configured() {
    with_isolated_storage("mirror-absent", |_db_path, storage| {
        let _unset_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "");
        let _unset_url = EnvGuard::set("PHASEGENT_REDMINE_REPOSITORY_URL", "");
        assert_eq!(auth::redmine_git_mirror_api_key(storage).unwrap(), None);
        assert_eq!(
            auth::redmine_repository_url_override(storage).unwrap(),
            None
        );
    });
}

#[test]
fn config_show_command_parses_without_role() {
    let args = ["config", "show"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("config show without --role must parse");
    match invocation.command {
        Command::ConfigShow => {}
        other => panic!("expected ConfigShow, got {other:?}"),
    }
}

#[test]
fn config_show_command_parses_with_role() {
    let args = ["--role", "executor", "config", "show"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("config show with --role must parse");
    match invocation.command {
        Command::ConfigShow => {}
        other => panic!("expected ConfigShow, got {other:?}"),
    }
}

#[test]
fn config_unknown_subcommand_is_rejected() {
    let args = ["config", "purge"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("unknown config subcommand must error");
    assert!(error.contains("purge"), "got: {error}");
}

#[test]
fn config_import_env_is_rejected() {
    // `config import-env` was removed; any attempt must be rejected as unknown command.
    for with_role in [true, false] {
        let mut args = Vec::new();
        if with_role {
            args.push("--role".to_owned());
            args.push("admin".to_owned());
        }
        args.push("config".to_owned());
        args.push("import-env".to_owned());
        let error = command::parse(&args).expect_err("import-env must be rejected");
        assert!(
            error.contains("unknown config command") && error.contains("import-env"),
            "got: {error}"
        );
    }
}

#[test]
fn config_set_parses_canonical_and_kebab_alias() {
    // Canonical and kebab-case alias must both be accepted and resolve to same canonical.
    // Project-id aliases were removed in Phase 1; they are asserted as
    // rejected in the dedicated regression test below.
    let cases = [
        ("PHASEGENT_API_BASE", "api-base"),
        (
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
            "redmine-git-mirror-api-key",
        ),
        ("PHASEGENT_DEFAULT_PROVIDER", "default-provider"),
        ("PHASEGENT_GITLAB_API_BASE", "gitlab-api-base"),
    ];
    for (canonical, alias) in cases {
        for name in [canonical, alias] {
            let is_secret = config_write::is_secret_setting(canonical);
            let args = if is_secret {
                vec![
                    "--role".to_owned(),
                    "executor".to_owned(),
                    "config".to_owned(),
                    "set".to_owned(),
                    name.to_owned(),
                    "--stdin".to_owned(),
                ]
            } else {
                // Non-secret global may not need role, but role-scoped does.
                // Use role for all to keep parser simple in this loop.
                let mut a = vec![
                    "--role".to_owned(),
                    "executor".to_owned(),
                    "config".to_owned(),
                    "set".to_owned(),
                    name.to_owned(),
                ];
                // For global default-provider without role, we test separately.
                if config_write::is_global_setting(canonical) {
                    // global case later
                }
                a.push("test-value".to_owned());
                a
            };
            // For global secret alias without role, test without role too.
            let invocation =
                command::parse(&args).unwrap_or_else(|e| panic!("set {name} must parse: {e}"));
            match invocation.command {
                Command::ConfigSet { setting, .. } => assert_eq!(setting, canonical),
                other => panic!("expected ConfigSet for {name}, got {other:?}"),
            }
        }
    }
}

#[test]
fn config_set_rejects_legacy_project_id_aliases() {
    // Phase 1: project-id persistence removed. The canonical names and
    // the ambiguous alias must be rejected as unknown settings at parse
    // time and via the config_write dispatch.
    for alias in [
        "PHASEGENT_REDMINE_PROJECT_ID",
        "redmine-project-id",
        "PHASEGENT_GITLAB_PROJECT_ID",
        "gitlab-project-id",
        "PHASEGENT_PROJECT_ID",
        "project-id",
        "project_id",
    ] {
        assert!(
            config_write::canonical_setting_name(alias).is_none(),
            "alias '{alias}' must be unknown after Phase 1"
        );
        let args = ["--role", "executor", "config", "set", alias, "42"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let error = command::parse(&args).expect_err("project-id alias must be rejected");
        assert!(error.contains("unknown config setting"), "got: {error}");
        let clear_args = ["--role", "executor", "config", "clear", alias]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let clear_error =
            command::parse(&clear_args).expect_err("clear project-id must be rejected");
        assert!(
            clear_error.contains("unknown config setting"),
            "got: {clear_error}"
        );
    }
    // Direct dispatch must also reject unknown canonicals.
    with_isolated_storage("project-id-rejected-dispatch", |_db_path, storage| {
        for canonical in [
            "PHASEGENT_REDMINE_PROJECT_ID",
            "PHASEGENT_GITLAB_PROJECT_ID",
            "PHASEGENT_PROJECT_ID",
        ] {
            let err =
                config_write::set_setting_value(Some(Role::Executor), canonical, "42", storage)
                    .unwrap_err();
            assert!(err.contains("unknown setting"), "got: {err}");
            let clear_err =
                config_write::clear_setting(Some(Role::Executor), canonical, storage).unwrap_err();
            assert!(clear_err.contains("unknown setting"), "got: {clear_err}");
        }
    });
}

#[test]
fn config_set_global_without_role_parses() {
    // Global settings must be usable without --role.
    let args = ["config", "set", "redmine-git-mirror-api-key", "--stdin"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("global set without --role must parse");
    match invocation.command {
        Command::ConfigSet { setting, stdin, .. } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY");
            assert!(stdin);
        }
        other => panic!("expected ConfigSet global without role, got {other:?}"),
    }
    let args = [
        "config",
        "set",
        "redmine-repository-url",
        "https://example.com",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("global set url without --role must parse");
    match invocation.command {
        Command::ConfigSet { setting, .. } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_REPOSITORY_URL")
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn config_set_role_scoped_requires_role() {
    let args = ["config", "set", "api-base", "https://example.com"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("role-scoped set without --role must error");
    assert!(error.contains("--role is required"), "got: {error}");
}

#[test]
fn config_set_rejects_secret_direct_value() {
    let args = [
        "--role",
        "executor",
        "config",
        "set",
        "redmine-git-mirror-api-key",
        "direct-secret-value",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("secret direct value must be rejected");
    assert!(
        error.contains("does not accept a direct value"),
        "got: {error}"
    );
    assert!(
        !error.contains("direct-secret-value"),
        "error must not echo secret: {error}"
    );
}

#[test]
fn config_set_rejects_unknown_setting() {
    let args = [
        "--role",
        "executor",
        "config",
        "set",
        "unknown-setting",
        "value",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("unknown setting must error");
    assert!(error.contains("unknown config setting"), "got: {error}");
    assert!(error.contains("unknown-setting"), "got: {error}");
}

#[test]
fn config_set_rejects_missing_value_for_non_secret() {
    let args = ["--role", "executor", "config", "set", "api-base"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("missing value must error");
    assert!(error.contains("requires a value"), "got: {error}");
}

#[test]
fn config_set_rejects_empty_value() {
    with_isolated_storage("set-empty", |_db_path, storage| {
        let err = config_write::set_setting_value(
            Some(Role::Executor),
            "PHASEGENT_API_BASE",
            "   ",
            storage,
        )
        .unwrap_err();
        assert!(err.contains("cannot be empty"), "got: {err}");
        // Secret empty via stdin helper
        let err = config_write::set_setting_stdin_content(
            None,
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
            "   ",
            storage,
        )
        .unwrap_err();
        assert!(err.contains("cannot be empty"), "got: {err}");
        // Ensure no secret leaked (empty is not secret, but check)
        assert!(!err.contains("shhh"), "secret leaked");
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
        // config show must redact
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
        // Output must use canonical name
        assert!(text.contains("PHASEGENT_API_BASE"));
        assert!(
            !text.contains("https://forgejo.example"),
            "value must not be echoed: {text}"
        );
        // Verify storage: generic api-base writes to three rows
        let forgejo = storage.load_role_config(Role::Executor).unwrap().unwrap();
        assert_eq!(forgejo.api_base.as_deref(), Some("https://forgejo.example"));
        let redmine = storage
            .load_redmine_config(Role::Executor)
            .unwrap()
            .unwrap();
        assert_eq!(redmine.api_base.as_deref(), Some("https://forgejo.example"));
        let gitlab = storage.load_gitlab_config(Role::Executor).unwrap().unwrap();
        assert_eq!(gitlab.api_base.as_deref(), Some("https://forgejo.example"));

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
            // Value not echoed? The outcome only contains setting, so can't leak.
            assert!(!text.contains(literal));
            let stored = storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .unwrap();
            assert_eq!(stored, literal);
        }
        // Invalid value
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
        // Global without role
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
        // Second clear should be false
        let outcome2 =
            config_write::clear_setting(None, "PHASEGENT_REDMINE_REPOSITORY_URL", storage).unwrap();
        assert!(
            serde_json::to_string(&outcome2)
                .unwrap()
                .contains("\"cleared\":false")
        );

        // Role-scoped clear requires role
        let err = config_write::clear_setting(None, "PHASEGENT_API_BASE", storage).unwrap_err();
        assert!(err.contains("--role is required"), "got: {err}");

        // Role-scoped clear via --role
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
        // Verify cleared
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
    let args = ["config", "clear", "redmine-git-mirror-api-key"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv = command::parse(&args).expect("clear global without role must parse");
    match inv.command {
        Command::ConfigClear { setting } => {
            assert_eq!(setting, "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        }
        other => panic!("got {other:?}"),
    }
    let args = ["config", "clear", "api-base"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let err = command::parse(&args).expect_err("clear role-scoped without role must error");
    assert!(err.contains("--role is required"), "got: {err}");

    let args = ["--role", "executor", "config", "clear", "api-base"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv = command::parse(&args).expect("clear with role must parse");
    match inv.command {
        Command::ConfigClear { setting } => assert_eq!(setting, "PHASEGENT_API_BASE"),
        other => panic!("got {other:?}"),
    }

    let args = ["--role", "executor", "config", "clear"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let err = command::parse(&args).expect_err("clear without setting must error");
    assert!(err.contains("requires a setting"), "got: {err}");

    let args = ["--role", "executor", "config", "clear", "unknown"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let err = command::parse(&args).expect_err("unknown clear setting must error");
    assert!(err.contains("unknown config setting"), "got: {err}");
}

#[test]
fn config_provider_get_parses_without_role() {
    let args = ["config", "provider", "get"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("config provider get without --role must parse");
    match invocation.command {
        Command::ConfigProviderGet => {}
        other => panic!("expected ConfigProviderGet, got {other:?}"),
    }
}

#[test]
fn config_provider_get_parses_with_role() {
    let args = ["--role", "executor", "config", "provider", "get"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("config provider get with --role must parse");
    match invocation.command {
        Command::ConfigProviderGet => {}
        other => panic!("expected ConfigProviderGet, got {other:?}"),
    }
}

#[test]
fn config_provider_set_parses_valid_values() {
    for (raw, expected) in [
        ("forgejo", crate::providers::ProviderKind::Forgejo),
        ("redmine", crate::providers::ProviderKind::Redmine),
        ("gitlab", crate::providers::ProviderKind::Gitlab),
    ] {
        let args = ["config", "provider", "set", raw]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let invocation = command::parse(&args)
            .unwrap_or_else(|error| panic!("config provider set {raw} must parse: {error}"));
        match invocation.command {
            Command::ConfigProviderSet { value } => assert_eq!(value, expected),
            other => panic!("expected ConfigProviderSet, got {other:?}"),
        }
    }
}

#[test]
fn config_provider_set_rejects_unknown_value() {
    let args = ["config", "provider", "set", "wrong"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("unknown provider value must error");
    assert!(
        error.contains("config provider set"),
        "error must name the operation: {error}"
    );
    assert!(
        error.contains("wrong"),
        "error must echo the offending value: {error}"
    );
}

#[test]
fn config_provider_set_rejects_missing_value() {
    let args = ["config", "provider", "set"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("missing value must error");
    assert!(error.contains("exactly one argument"), "got: {error}");
}

#[test]
fn config_provider_set_rejects_extra_arguments() {
    let args = ["config", "provider", "set", "redmine", "extra"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("extra arguments must error");
    assert!(error.contains("exactly one argument"), "got: {error}");
}

#[test]
fn config_provider_clear_parses_without_role() {
    let args = ["config", "provider", "clear"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation =
        command::parse(&args).expect("config provider clear without --role must parse");
    match invocation.command {
        Command::ConfigProviderClear => {}
        other => panic!("expected ConfigProviderClear, got {other:?}"),
    }
}

#[test]
fn config_provider_clear_rejects_extra_arguments() {
    let args = ["config", "provider", "clear", "extra"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("extra arguments must error");
    assert!(error.contains("no arguments"), "got: {error}");
}

#[test]
fn config_provider_unknown_subcommand_is_rejected() {
    let args = ["config", "provider", "purge"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("unknown config provider subcommand must error");
    assert!(
        error.contains("unknown config provider command"),
        "got: {error}"
    );
}

#[test]
fn config_show_includes_gitlab_fields_without_leaking_token() {
    with_isolated_storage("show-gitlab", |_db_path, storage| {
        storage
            .save_credential(Role::Executor, PROVIDER_GITLAB, "gitlab-private-token-shhh")
            .unwrap();
        storage
            .save_gitlab_config(
                Role::Executor,
                &crate::auth::GitlabStoredConfig {
                    api_base: Some("https://gitlab.example".to_owned()),
                    project_id: Some(42),
                },
            )
            .unwrap();
        storage
            .save_credential(Role::Executor, PROVIDER_FORGEJO, "forgejo-secret-token")
            .unwrap();
        storage
            .save_credential(Role::Executor, PROVIDER_REDMINE, "redmine-secret-key")
            .unwrap();

        let snapshot = config::show(Some(Role::Executor), storage).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();

        for forbidden in [
            "gitlab-private-token-shhh",
            "forgejo-secret-token",
            "redmine-secret-key",
        ] {
            assert!(
                !text.contains(forbidden),
                "snapshot leaked '{forbidden}': {text}"
            );
        }

        let roles = snapshot["roles"].as_array().expect("roles array");
        assert_eq!(roles.len(), 1);
        let executor = &roles[0];
        assert_eq!(executor["role"], "executor");
        assert_eq!(
            executor["gitlab_api_base"].as_str(),
            Some("https://gitlab.example")
        );
        // Project-id fields were removed in Phase 1; stored values are
        // ignored and must not appear in the snapshot.
        assert!(
            executor.get("gitlab_project_id").is_none(),
            "snapshot must not expose gitlab_project_id after Phase 1: {executor:?}"
        );
        assert!(
            executor.get("redmine_project_id").is_none(),
            "snapshot must not expose redmine_project_id after Phase 1: {executor:?}"
        );
        assert_eq!(executor["gitlab_credential"]["present"], Value::Bool(true));
        assert_eq!(
            executor["gitlab_credential"]["length"],
            Value::from("gitlab-private-token-shhh".len())
        );
        assert_eq!(executor["forgejo_credential"]["present"], Value::Bool(true));
        assert_eq!(executor["redmine_credential"]["present"], Value::Bool(true));
        // Verify legacy stored project_id was ignored, not leaked.
        let stored = storage.load_gitlab_config(Role::Executor).unwrap().unwrap();
        assert_eq!(
            stored.project_id, None,
            "legacy gitlab project_id must be inert (load returns None)"
        );
    });
}

#[test]
fn gitlab_config_snapshot_omits_unset_fields() {
    with_isolated_storage("show-gitlab-empty", |_db_path, storage| {
        let snapshot = config::show(Some(Role::Executor), storage).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();
        let roles = snapshot["roles"].as_array().expect("roles array");
        assert_eq!(roles.len(), 1);
        let executor = &roles[0];
        assert!(executor["gitlab_api_base"].is_null());
        assert!(
            executor.get("gitlab_project_id").is_none(),
            "gitlab_project_id must be absent after Phase 1: {executor:?}"
        );
        assert!(
            executor.get("redmine_project_id").is_none(),
            "redmine_project_id must be absent after Phase 1: {executor:?}"
        );
        assert_eq!(executor["gitlab_credential"]["present"], Value::Bool(false));
        assert!(
            executor["gitlab_credential"]["length"].is_null(),
            "zero-length credential summary must omit the length slot: {executor:?}"
        );
        assert!(
            text.contains("gitlab_api_base"),
            "snapshot must name gitlab_api_base: {text}"
        );
        assert!(
            !text.contains("gitlab_project_id"),
            "snapshot must not contain gitlab_project_id after Phase 1: {text}"
        );
        assert!(
            !text.contains("redmine_project_id"),
            "snapshot must not contain redmine_project_id after Phase 1: {text}"
        );
        assert!(
            text.contains("gitlab_credential"),
            "snapshot must name gitlab_credential: {text}"
        );
    });
}

#[test]
fn legacy_project_id_values_are_inert_and_not_resolved() {
    with_isolated_storage("legacy-project-id-inert", |db_path, storage| {
        // Simulate a legacy database where project ids were persisted
        // before Phase 1 by writing directly via SQL before the
        // migration runs.
        storage
            .connection
            .execute(
                "INSERT INTO role_redmine_config (role, api_base, project_id, close_status_id) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(role) DO UPDATE SET api_base=excluded.api_base, project_id=excluded.project_id, close_status_id=excluded.close_status_id",
                rusqlite::params!["executor", "https://redmine.example", "legacy-redmine-id", 5_i64],
            )
            .unwrap();
        storage
            .connection
            .execute(
                "INSERT INTO role_gitlab_config (role, api_base, project_id) VALUES (?1, ?2, ?3) ON CONFLICT(role) DO UPDATE SET api_base=excluded.api_base, project_id=excluded.project_id",
                rusqlite::params!["executor", "https://gitlab.example", 99_i64],
            )
            .unwrap();
        // Re-open to trigger the migration that clears legacy values.
        let reopened = Storage::open_at(db_path).unwrap();
        let redmine = reopened
            .load_redmine_config(Role::Executor)
            .unwrap()
            .unwrap();
        assert_eq!(
            redmine.project_id, None,
            "redmine legacy project_id must be inert"
        );
        assert_eq!(redmine.api_base.as_deref(), Some("https://redmine.example"));
        assert_eq!(redmine.close_status_id, Some(5));
        let gitlab = reopened
            .load_gitlab_config(Role::Executor)
            .unwrap()
            .unwrap();
        assert_eq!(
            gitlab.project_id, None,
            "gitlab legacy project_id must be inert"
        );
        assert_eq!(gitlab.api_base.as_deref(), Some("https://gitlab.example"));
        // Verify raw column is NULL after migration.
        let redmine_raw: Option<String> = reopened
            .connection
            .query_row(
                "SELECT project_id FROM role_redmine_config WHERE role='executor'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            redmine_raw.is_none(),
            "raw redmine project_id column must be NULL: {redmine_raw:?}"
        );
        let gitlab_raw: Option<i64> = reopened
            .connection
            .query_row(
                "SELECT project_id FROM role_gitlab_config WHERE role='executor'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            gitlab_raw.is_none(),
            "raw gitlab project_id column must be NULL: {gitlab_raw:?}"
        );

        // Provider resolution must not use legacy values: Redmine without
        // explicit --project-id must have None, GitLab without explicit
        // must error even though legacy row existed.
        let _env_redmine = EnvGuard::set("PHASEGENT_REDMINE_PROJECT_ID", "env-id");
        let _env_gitlab = EnvGuard::set("PHASEGENT_GITLAB_PROJECT_ID", "123");
        let _env_generic = EnvGuard::set("PHASEGENT_PROJECT_ID", "generic-id");
        let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
        // Redmine: explicit None, env present, but Phase 1 ignores env.
        let redmine_config = crate::providers::config::RedmineConfig::resolve(
            Role::Executor,
            Some("https://redmine.example"),
            None,
            Some("5"),
        )
        .unwrap();
        assert_eq!(
            redmine_config.project_id, None,
            "redmine env must be ignored after Phase 1"
        );
        // GitLab: explicit None, env present, must still error.
        let gitlab_err = crate::providers::config::GitlabConfig::resolve(
            Role::Executor,
            Some("https://gitlab.example"),
            None,
        )
        .unwrap_err();
        assert!(
            gitlab_err.to_string().contains("not configured"),
            "gitlab must require explicit project-id: {gitlab_err}"
        );
        // Explicit project-id still wins.
        let redmine_explicit = crate::providers::config::RedmineConfig::resolve(
            Role::Executor,
            Some("https://redmine.example"),
            Some("explicit-42"),
            Some("5"),
        )
        .unwrap();
        assert_eq!(redmine_explicit.project_id.as_deref(), Some("explicit-42"));
        let gitlab_explicit = crate::providers::config::GitlabConfig::resolve(
            Role::Executor,
            Some("https://gitlab.example"),
            Some("77"),
        )
        .unwrap();
        assert_eq!(gitlab_explicit.project_id, 77);
    });
}

#[test]
fn storage_global_default_provider_save_load_and_delete_round_trip() {
    with_isolated_storage("global-default-crud", |_db_path, storage| {
        assert!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .is_none(),
            "fresh storage must report an absent default"
        );
        assert!(
            !storage
                .delete_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap(),
            "delete on an absent row must return false"
        );

        storage
            .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_REDMINE)
            .unwrap();
        assert_eq!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .as_deref(),
            Some(PROVIDER_REDMINE)
        );

        assert!(
            storage
                .delete_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap(),
            "delete on an existing row must return true"
        );
        assert!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .is_none(),
            "delete must leave the row absent"
        );
    });
}

#[test]
fn config_provider_set_get_and_clear_round_trip_through_helpers() {
    with_isolated_storage("global-default-helpers", |_db_path, storage| {
        let initial = config::provider_get(storage).unwrap();
        assert!(
            initial.provider.is_none(),
            "fresh storage must report null default: {initial:?}"
        );

        for literal in [PROVIDER_FORGEJO, PROVIDER_REDMINE, PROVIDER_GITLAB] {
            let outcome = config::provider_set(literal, storage).unwrap();
            assert_eq!(outcome.provider, Some(literal));
            let stored = config::provider_get(storage).unwrap();
            assert_eq!(stored.provider, Some(literal));
        }

        let cleared = config::provider_clear(storage).unwrap();
        assert!(cleared.cleared, "first clear must remove the row");
        let cleared_again = config::provider_clear(storage).unwrap();
        assert!(!cleared_again.cleared, "second clear must be a no-op");
        assert!(
            config::provider_get(storage).unwrap().provider.is_none(),
            "clear must leave the default unset"
        );
    });
}

#[test]
fn config_provider_set_helper_rejects_unknown_value() {
    with_isolated_storage("global-default-invalid", |_db_path, storage| {
        let error = config::provider_set("wrong", storage).unwrap_err();
        assert!(
            error.contains("invalid provider"),
            "error must name the offending value: {error}"
        );
        assert!(
            error.contains("wrong"),
            "error must echo the value: {error}"
        );

        assert!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .is_none(),
            "rejected set must not persist anything"
        );
    });
}

#[test]
fn config_provider_get_rejects_stale_invalid_row() {
    with_isolated_storage("global-default-stale", |_db_path, storage| {
        storage
            .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "wrong")
            .unwrap();

        let error = config::provider_get(storage).unwrap_err();
        assert!(
            error.contains("persisted PHASEGENT_DEFAULT_PROVIDER is invalid"),
            "error must identify the persisted row: {error}"
        );
        assert!(
            error.contains("wrong"),
            "error must echo the offending value: {error}"
        );
    });
}

#[test]
fn config_show_reports_global_default_provider_without_secrets() {
    with_isolated_storage("show-global-default", |_db_path, storage| {
        let _unset_default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "");

        config::provider_set(PROVIDER_GITLAB, storage).unwrap();

        let snapshot = config::show(None, storage).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            snapshot["global_default_provider"].as_str(),
            Some(PROVIDER_GITLAB),
            "snapshot must surface the machine-wide default"
        );

        let settings = snapshot["global_settings"].as_array().expect("settings");
        let entry = settings
            .iter()
            .find(|entry| entry["name"] == "PHASEGENT_DEFAULT_PROVIDER")
            .expect("global default entry");
        assert_eq!(entry["present"], Value::Bool(true));
        assert_eq!(
            entry["value"].as_str(),
            Some(PROVIDER_GITLAB),
            "non-secret slot must carry the literal: {entry:?}"
        );
        assert!(
            !text.contains("mirror-bearer-secret"),
            "snapshot must never leak secret values: {text}"
        );

        config::provider_clear(storage).unwrap();
        let unset = config::show(None, storage).unwrap();
        let unset_text = serde_json::to_string(&unset).unwrap();
        assert!(
            unset["global_default_provider"].is_null(),
            "absent default must render as null: {unset:?}"
        );
        let settings = unset["global_settings"].as_array().expect("settings");
        let entry = settings
            .iter()
            .find(|entry| entry["name"] == "PHASEGENT_DEFAULT_PROVIDER")
            .expect("global default entry");
        assert_eq!(entry["present"], Value::Bool(false));
        assert!(entry["value"].is_null());
        assert!(unset_text.contains("global_default_provider"));
    });
}

#[test]
fn project_list_parses_without_project_id() {
    // Redmine project list must work without --project-id; it is the
    // discovery path for another checkout.
    let args = [
        "--role",
        "executor",
        "--provider",
        "redmine",
        "project",
        "list",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("project list without --project-id must parse");
    match invocation.command {
        Command::Project(ProjectCommand::List) => {}
        other => panic!("expected Project List, got {other:?}"),
    }
    // Also without role? No, project list requires role via top-level parser, but not project-id.
    // With explicit project-id should also parse.
    let args = [
        "--role",
        "executor",
        "--provider",
        "redmine",
        "--project-id",
        "42",
        "project",
        "list",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("project list with --project-id must parse");
    assert!(matches!(
        invocation.command,
        Command::Project(ProjectCommand::List)
    ));
    assert_eq!(invocation.project_id.as_deref(), Some("42"));

    // Ensure help mentions no project-id needed
    let help_args = ["--role", "executor", "--help", "project", "list"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let inv = command::parse(&help_args).expect("help must parse");
    match inv.command {
        Command::Help(crate::command::HelpTopic::ProjectCommand(cmd)) => assert_eq!(cmd, "list"),
        other => panic!("expected help topic for project list, got {other:?}"),
    }
}
