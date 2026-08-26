//! Focused tests for the `config show` and `config import-env` flows.
//!
//! These tests cover the phase 2 acceptance criteria that require
//! focused coverage:
//!
//! * `config show` redacts credentials (never echoes secret content
//!   or URL userinfo / query / fragment).
//! * `config import-env` persists role-scoped and global settings,
//!   reports counts and per-name flags, and never prints the values
//!   it persisted.
//! * The mirror fallback prefers the environment variable and only
//!   reads SQLite when the env var is unset.
//! * Ordinary provider commands must not implicitly persist
//!   environment values; the explicit `import-env` invocation is the
//!   only path that writes.
//! * Phase `global-provider-default`: `config provider get / set /
//!   clear` manage the machine-wide default provider; the resolver
//!   honours the documented precedence; the snapshot surfaces the
//!   default without leaking secrets.
//!
//! Tests use a private temp home so they cannot mutate the operator's
//! `~/.config/opencode/phasegent` database.

use crate::auth;
use crate::command::{self, Command};
use crate::config;
use crate::policy::Role;
use crate::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::storage::{PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE, Storage};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

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

fn with_isolated_home<T>(label: &str, f: impl FnOnce(&PathBuf) -> T) -> T {
    let _lock = lock_workflow_tests();
    let home = unique_temp_dir(label);
    let _home = EnvGuard::set("HOME", home.to_string_lossy().as_ref());
    let result = f(&home);
    let _ = fs::remove_dir_all(home);
    result
}

#[test]
fn config_show_redacts_credentials_and_sanitises_url() {
    with_isolated_home("show-redact", |_home| {
        let storage = Storage::open().unwrap();
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

        let snapshot = config::show(Some(Role::Executor)).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();

        // The redacted snapshot must never echo secret content.
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

        // The URL userinfo, query, and fragment are stripped before the
        // snapshot is rendered, but the sanitised host and path remain.
        assert!(text.contains("hush.example.com"));
        assert!(!text.contains("?token="));
        assert!(!text.contains("#fragment"));
        assert!(!text.contains("user:"));

        // The credential summary reports presence and length, not the
        // value. The mirror key summary follows the same rule.
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
    // Regression test for review P1: unparseable URLs that still
    // embed credential-looking substrings must be rendered as the
    // safe placeholder rather than echoing the raw input. The
    // persisted value is still stored verbatim (the user can decide
    // to remove it) but the snapshot never returns a string that
    // could leak credentials.
    with_isolated_home("show-bad-url-redact", |_home| {
        let storage = Storage::open().unwrap();
        let malicious_inputs = [
            "git@user:password@host.example.com:owner/repo.git",
            "https://user:pa$$word@example.com:owner/repo.git",
            "https://user:password@example.com:notaport/path",
        ];
        storage
            .save_global_setting("PHASEGENT_REDMINE_REPOSITORY_URL", malicious_inputs[0])
            .unwrap();

        let snapshot = config::show(None).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();

        // The persisted value is reported by length and the
        // sanitised slot uses the placeholder; none of the
        // credential-looking substrings may appear in the output.
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
    with_isolated_home("show-global", |_home| {
        let storage = Storage::open().unwrap();
        for role in [
            Role::Admin,
            Role::Orchestrator,
            Role::Executor,
            Role::Reviewer,
        ] {
            let config = crate::auth::StoredConfig {
                provider: Some(PROVIDER_FORGEJO.to_owned()),
                ..Default::default()
            };
            storage.save_role_config(role, &config).unwrap();
        }

        let snapshot = config::show(None).unwrap();
        let roles = snapshot["roles"].as_array().expect("roles array");
        let names: Vec<&str> = roles
            .iter()
            .map(|entry| entry["role"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            vec!["admin", "orchestrator", "executor", "reviewer"],
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
fn import_env_persists_role_scoped_and_global_settings_and_reports_counts() {
    with_isolated_home("import-env", |_home| {
        // Seed the environment with a deliberate mix: every variable
        // the import flow understands is set to a non-empty value so
        // the persistence path runs end-to-end. Empty values are
        // covered by `import_env_skips_unset_environment_variables`.
        let _provider = EnvGuard::set("PHASEGENT_PROVIDER", "redmine");
        let _api_base = EnvGuard::set("PHASEGENT_API_BASE", "https://forgejo.example");
        let _repository = EnvGuard::set("PHASEGENT_REPOSITORY", "owner/repo");
        let _redmine_api = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", "https://redmine.example");
        let _redmine_project = EnvGuard::set("PHASEGENT_REDMINE_PROJECT_ID", "42");
        let _redmine_status = EnvGuard::set("PHASEGENT_REDMINE_CLOSE_STATUS_ID", "7");
        let _mirror_key = EnvGuard::set(
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
            "mirror-bearer-secret",
        );
        let _mirror_url = EnvGuard::set(
            "PHASEGENT_REDMINE_REPOSITORY_URL",
            "https://mirror.example/owner/repo.git",
        );
        // Generic aliases are intentionally set to verify they land
        // on the Redmine row too. The Redmine-specific
        // project_id will be overwritten by PHASEGENT_PROJECT_ID in
        // the persistence loop, so we choose a different value to
        // make the test deterministic.
        let _generic_project = EnvGuard::set("PHASEGENT_PROJECT_ID", "84");
        let _generic_status = EnvGuard::set("PHASEGENT_CLOSE_STATUS_ID", "9");

        let outcome = config::import_env(Role::Executor).unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        // The JSON must never echo a secret value.
        assert!(!text.contains("mirror-bearer-secret"));

        let imported_names: Vec<&str> = outcome
            .role_scoped
            .iter()
            .chain(outcome.global_settings.iter())
            .filter(|entry| entry.imported)
            .map(|entry| entry.name)
            .collect();
        for expected in [
            "PHASEGENT_PROVIDER",
            "PHASEGENT_API_BASE",
            "PHASEGENT_REPOSITORY",
            "PHASEGENT_REDMINE_API_BASE",
            "PHASEGENT_PROJECT_ID",
            "PHASEGENT_CLOSE_STATUS_ID",
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
            "PHASEGENT_REDMINE_REPOSITORY_URL",
        ] {
            assert!(
                imported_names.contains(&expected),
                "import-env must report '{expected}' as imported: {imported_names:?}"
            );
        }
        assert!(
            outcome.imported >= 9,
            "expected at least nine fields to land in SQLite, got {}",
            outcome.imported
        );

        // The mirror bearer entry must be flagged as a secret so the
        // CLI layer can render its presence/length without exposing it.
        let mirror_entry = outcome
            .global_settings
            .iter()
            .find(|entry| entry.name == "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
            .expect("mirror key entry");
        assert!(
            mirror_entry.secret,
            "mirror bearer entry must be flagged as a secret"
        );
        assert!(mirror_entry.imported);

        // Storage must reflect the persisted state.
        let storage = Storage::open().unwrap();
        let stored_key = storage
            .load_global_setting("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
            .unwrap()
            .expect("mirror key persisted");
        assert_eq!(stored_key, "mirror-bearer-secret");
        let stored_url = storage
            .load_global_setting("PHASEGENT_REDMINE_REPOSITORY_URL")
            .unwrap()
            .expect("mirror url persisted");
        assert_eq!(stored_url, "https://mirror.example/owner/repo.git");

        let role = storage.load_role_config(Role::Executor).unwrap().unwrap();
        assert_eq!(role.provider.as_deref(), Some("redmine"));
        assert_eq!(role.api_base.as_deref(), Some("https://forgejo.example"));
        assert_eq!(role.repository.as_deref(), Some("owner/repo"));

        let redmine = storage
            .load_redmine_config(Role::Executor)
            .unwrap()
            .unwrap();
        // `PHASEGENT_REDMINE_API_BASE` is processed after the generic
        // `PHASEGENT_API_BASE` in `import_env`, so the Redmine row
        // observes the provider-specific value rather than the
        // generic one. The Forgejo row still carries the generic base
        // so a Forgejo fallback can use it.
        assert_eq!(redmine.api_base.as_deref(), Some("https://redmine.example"));
        // When both `PHASEGENT_REDMINE_PROJECT_ID` and the generic
        // alias `PHASEGENT_PROJECT_ID` are set, `import-env` writes
        // both into the same column and the later write wins. The
        // resolver still prefers the provider-specific env var at
        // runtime, so the persisted value is only consulted when the
        // shell no longer carries the variable.
        assert_eq!(redmine.project_id.as_deref(), Some("84"));
        assert_eq!(redmine.close_status_id, Some(9));
    });
}

#[test]
fn import_env_persists_generic_api_base_to_both_role_rows() {
    // Regression test for review P1: `PHASEGENT_API_BASE` must land
    // on both `role_config.api_base` (Forgejo) and
    // `role_redmine_config.api_base` (Redmine) so a Redmine
    // `RedmineConfig::resolve()` whose env vars are unset can still
    // fall back to the SQLite value.
    with_isolated_home("import-env-generic-api-base", |_home| {
        let _api_base = EnvGuard::set("PHASEGENT_API_BASE", "https://shared.example");

        config::import_env(Role::Executor).unwrap();

        let storage = Storage::open().unwrap();
        let forgejo_row = storage.load_role_config(Role::Executor).unwrap().unwrap();
        assert_eq!(
            forgejo_row.api_base.as_deref(),
            Some("https://shared.example"),
            "generic PHASEGENT_API_BASE must land on the Forgejo row"
        );
        let redmine_row = storage
            .load_redmine_config(Role::Executor)
            .unwrap()
            .unwrap();
        assert_eq!(
            redmine_row.api_base.as_deref(),
            Some("https://shared.example"),
            "generic PHASEGENT_API_BASE must also land on the Redmine row so RedmineConfig::resolve can fall back"
        );
    });
}

#[test]
fn import_env_redmine_specific_api_base_overrides_generic() {
    // When both the generic and the Redmine-specific API base are
    // set, the Redmine row must observe the provider-specific value
    // so the resolver's documented precedence is preserved end to
    // end. The Forgejo row keeps the generic value because the
    // Redmine-specific override only affects role_redmine_config.
    with_isolated_home("import-env-redmine-specific-wins", |_home| {
        let _generic = EnvGuard::set("PHASEGENT_API_BASE", "https://shared.example");
        let _specific = EnvGuard::set("PHASEGENT_REDMINE_API_BASE", "https://redmine.example");

        config::import_env(Role::Executor).unwrap();

        let storage = Storage::open().unwrap();
        let forgejo_row = storage.load_role_config(Role::Executor).unwrap().unwrap();
        assert_eq!(
            forgejo_row.api_base.as_deref(),
            Some("https://shared.example"),
            "Forgejo row keeps the generic API base"
        );
        let redmine_row = storage
            .load_redmine_config(Role::Executor)
            .unwrap()
            .unwrap();
        assert_eq!(
            redmine_row.api_base.as_deref(),
            Some("https://redmine.example"),
            "Redmine-specific API base must override the generic value on the Redmine row"
        );
    });
}

#[test]
fn import_env_skips_unset_environment_variables() {
    with_isolated_home("import-env-skipped", |_home| {
        // Only one role-scoped variable is set so the other entries
        // are reported as skipped.
        let _api_base = EnvGuard::set("PHASEGENT_API_BASE", "https://forgejo.example");

        let outcome = config::import_env(Role::Admin).unwrap();
        assert!(outcome.imported >= 1);
        assert!(outcome.skipped >= 1);
        let skipped_names: Vec<&str> = outcome
            .role_scoped
            .iter()
            .filter(|entry| !entry.imported)
            .map(|entry| entry.name)
            .collect();
        assert!(
            skipped_names.contains(&"PHASEGENT_REPOSITORY"),
            "unset environment variables must appear in the skipped list: {skipped_names:?}"
        );
    });
}

#[test]
fn ordinary_provider_commands_do_not_persist_env_values() {
    // `resolve_kind` reads `PHASEGENT_PROVIDER` from the environment
    // to select a provider; the storage layer must not record the
    // value as a side effect. We assert this by checking that
    // `role_config.provider` remains unset after the resolver runs.
    with_isolated_home("no-implicit-persist", |_home| {
        let _provider = EnvGuard::set("PHASEGENT_PROVIDER", "redmine");

        // Run a typical chain: resolve_kind (cli.rs path) followed by
        // loading the role config from storage. The pre-set SQLite
        // state must be unchanged.
        let resolved = crate::provider_config::resolve_kind(Role::Executor, None).unwrap();
        assert_eq!(resolved.as_str(), "redmine");

        let storage = Storage::open().unwrap();
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
    with_isolated_home("mirror-fallback", |_home| {
        let storage = Storage::open().unwrap();
        storage
            .save_global_setting("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "sqlite-bearer-key")
            .unwrap();
        storage
            .save_global_setting(
                "PHASEGENT_REDMINE_REPOSITORY_URL",
                "https://sqlite.example/owner/repo.git",
            )
            .unwrap();

        // When the environment is empty, the resolver must return the
        // SQLite value.
        let _unset_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "");
        let _unset_url = EnvGuard::set("PHASEGENT_REDMINE_REPOSITORY_URL", "");
        assert_eq!(
            auth::redmine_git_mirror_api_key().unwrap(),
            Some("sqlite-bearer-key".to_owned())
        );
        assert_eq!(
            auth::redmine_repository_url_override().unwrap(),
            Some("https://sqlite.example/owner/repo.git".to_owned())
        );

        // When the environment variable is set, it must win over
        // SQLite.
        let _env_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "env-bearer-key");
        let _env_url = EnvGuard::set(
            "PHASEGENT_REDMINE_REPOSITORY_URL",
            "https://env.example/owner/repo.git",
        );
        assert_eq!(
            auth::redmine_git_mirror_api_key().unwrap(),
            Some("env-bearer-key".to_owned())
        );
        assert_eq!(
            auth::redmine_repository_url_override().unwrap(),
            Some("https://env.example/owner/repo.git".to_owned())
        );
    });
}

#[test]
fn mirror_fallback_returns_none_when_no_source_is_configured() {
    with_isolated_home("mirror-absent", |_home| {
        let _unset_key = EnvGuard::set("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY", "");
        let _unset_url = EnvGuard::set("PHASEGENT_REDMINE_REPOSITORY_URL", "");
        assert_eq!(auth::redmine_git_mirror_api_key().unwrap(), None);
        assert_eq!(auth::redmine_repository_url_override().unwrap(), None);
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
fn config_import_env_requires_role() {
    let args = ["config", "import-env"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let error = command::parse(&args).expect_err("config import-env must require --role");
    assert!(error.contains("--role is required"), "got: {error}");
}

#[test]
fn config_import_env_parses_with_role() {
    let args = ["--role", "admin", "config", "import-env"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("config import-env with --role must parse");
    match invocation.command {
        Command::ConfigImportEnv => {}
        other => panic!("expected ConfigImportEnv, got {other:?}"),
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
fn config_provider_get_parses_without_role() {
    // `config provider get` is machine-wide so the parser must not
    // require --role; the global default lives in
    // `global_setting`, not in a per-role row.
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
    // --role is harmless on these subcommands because the global
    // default is machine-wide; supplying --role must not error.
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
        ("forgejo", crate::provider::ProviderKind::Forgejo),
        ("redmine", crate::provider::ProviderKind::Redmine),
        ("gitlab", crate::provider::ProviderKind::Gitlab),
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
    // Phase-1 GitLab foundation: the per-role snapshot must report
    // the new gitlab_api_base, gitlab_project_id, and
    // gitlab_credential fields. The PRIVATE-TOKEN value must never
    // appear in the rendered JSON; only presence/length survives,
    // matching the redmine/forgejo convention.
    with_isolated_home("show-gitlab", |_home| {
        let storage = Storage::open().unwrap();
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

        let snapshot = config::show(Some(Role::Executor)).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();

        // No secret value survives the snapshot.
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
        assert_eq!(executor["gitlab_project_id"].as_u64(), Some(42));
        // Credential summary reports presence and length only.
        assert_eq!(executor["gitlab_credential"]["present"], Value::Bool(true));
        assert_eq!(
            executor["gitlab_credential"]["length"],
            Value::from("gitlab-private-token-shhh".len())
        );
        // Forgejo and redmine fields continue to be reported as
        // before so the snapshot stays a single object per role.
        assert_eq!(executor["forgejo_credential"]["present"], Value::Bool(true));
        assert_eq!(executor["redmine_credential"]["present"], Value::Bool(true));
    });
}

#[test]
fn import_env_persists_gitlab_env_values_to_role_gitlab_config() {
    // Phase-1 GitLab foundation: `config import-env` must persist the
    // provider-specific GitLab env vars to the role_gitlab_config
    // row and never echo the underlying values into its JSON report.
    // The generic `PHASEGENT_PROJECT_ID` alias is also parsed
    // numerically and lands on the Gitlab row so the documented
    // resolver fallback path keeps working after a restart.
    with_isolated_home("import-env-gitlab", |_home| {
        let _provider = EnvGuard::set("PHASEGENT_PROVIDER", "gitlab");
        let _gitlab_api = EnvGuard::set("PHASEGENT_GITLAB_API_BASE", "https://gitlab.example");
        let _gitlab_project = EnvGuard::set("PHASEGENT_GITLAB_PROJECT_ID", "42");
        let _generic_project = EnvGuard::set("PHASEGENT_PROJECT_ID", "84");

        let outcome = config::import_env(Role::Executor).unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        // The JSON report never carries an environment variable
        // value, only the import flag — keep that invariant.
        assert!(!text.contains("https://gitlab.example"));

        let imported_names: Vec<&str> = outcome
            .role_scoped
            .iter()
            .chain(outcome.global_settings.iter())
            .filter(|entry| entry.imported)
            .map(|entry| entry.name)
            .collect();
        for expected in [
            "PHASEGENT_PROVIDER",
            "PHASEGENT_GITLAB_API_BASE",
            "PHASEGENT_GITLAB_PROJECT_ID",
            "PHASEGENT_PROJECT_ID",
        ] {
            assert!(
                imported_names.contains(&expected),
                "import-env must report '{expected}' as imported: {imported_names:?}"
            );
        }

        let storage = Storage::open().unwrap();
        let provider = storage
            .load_role_config(Role::Executor)
            .unwrap()
            .expect("role_config row must exist after import")
            .provider;
        assert_eq!(provider.as_deref(), Some(PROVIDER_GITLAB));

        let gitlab = storage
            .load_gitlab_config(Role::Executor)
            .unwrap()
            .expect("role_gitlab_config row must exist after import");
        assert_eq!(gitlab.api_base.as_deref(), Some("https://gitlab.example"));
        // PHASEGENT_PROJECT_ID (the generic alias) is processed
        // last and wins over PHASEGENT_GITLAB_PROJECT_ID. Both are
        // parsed numerically and the alias lands on the Gitlab row
        // so the resolver's env-over-SQLite fallback continues to
        // work after a restart.
        assert_eq!(gitlab.project_id, Some(84));
    });
}

#[test]
fn import_env_rejects_non_numeric_gitlab_project_id() {
    // PHASEGENT_GITLAB_PROJECT_ID requires a numeric value because
    // GitLab identifiers are always positive integers. The
    // parser-level rejection keeps the error actionable instead of
    // surfacing as a generic runtime failure at provider build time.
    with_isolated_home("import-env-gitlab-bad-id", |_home| {
        let _bad_id = EnvGuard::set("PHASEGENT_GITLAB_PROJECT_ID", "not-a-number");
        let error = config::import_env(Role::Executor).unwrap_err();
        assert!(
            error.contains("PHASEGENT_GITLAB_PROJECT_ID"),
            "error must name the offending env var: {error}"
        );
        assert!(
            error.contains("not-a-number"),
            "error must echo the offending value for the operator: {error}"
        );
    });
}

#[test]
fn import_env_rejects_zero_gitlab_project_id() {
    // GitLab identifiers must be greater than zero; the same guard
    // matches the storage-layer bootstrap check so a hostile env
    // export cannot silently land a zero id.
    with_isolated_home("import-env-gitlab-zero", |_home| {
        let _zero = EnvGuard::set("PHASEGENT_GITLAB_PROJECT_ID", "0");
        let error = config::import_env(Role::Executor).unwrap_err();
        assert!(
            error.contains("greater than zero"),
            "zero gitlab project id must be rejected: {error}"
        );
    });
}

#[test]
fn gitlab_config_snapshot_omits_unset_fields() {
    // Operators frequently run `config show` before `auth setup`
    // has populated the GitLab row; the snapshot must keep rendering
    // cleanly (no false positives, no leaked fields) when the
    // gitlab_api_base / gitlab_project_id / gitlab_credential
    // columns are all empty.
    with_isolated_home("show-gitlab-empty", |_home| {
        let snapshot = config::show(Some(Role::Executor)).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();
        let roles = snapshot["roles"].as_array().expect("roles array");
        assert_eq!(roles.len(), 1);
        let executor = &roles[0];
        assert!(executor["gitlab_api_base"].is_null());
        assert!(executor["gitlab_project_id"].is_null());
        assert_eq!(executor["gitlab_credential"]["present"], Value::Bool(false));
        // The CredentialSummary serde skips length when it is zero
        // (matching the existing forgejo/redmine convention), so an
        // absent length field is the expected outcome; downstream
        // tooling already treats `present: false` as "no credential"
        // without needing the length slot.
        assert!(
            executor["gitlab_credential"]["length"].is_null(),
            "zero-length credential summary must omit the length slot: {executor:?}"
        );
        // Make sure the new fields are at least named in the rendered
        // JSON so downstream tooling can switch on them safely.
        assert!(
            text.contains("gitlab_api_base"),
            "snapshot must name gitlab_api_base: {text}"
        );
        assert!(
            text.contains("gitlab_project_id"),
            "snapshot must name gitlab_project_id: {text}"
        );
        assert!(
            text.contains("gitlab_credential"),
            "snapshot must name gitlab_credential: {text}"
        );
    });
}

#[test]
fn storage_global_default_provider_save_load_and_delete_round_trip() {
    // Phase `global-provider-default`: the storage layer exposes
    // save / load / delete on the `PHASEGENT_DEFAULT_PROVIDER`
    // global setting row. The delete helper must report whether a
    // row was actually removed so the `config provider clear`
    // command can distinguish "cleared an existing default" from
    // "no-op because the default was already absent".
    with_isolated_home("global-default-crud", |_home| {
        let storage = Storage::open().unwrap();
        // No row exists yet — load returns None and delete returns
        // false to signal the absent default.
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
    // Phase `global-provider-default`: the `config` facade owns the
    // `provider_get`, `provider_set`, and `provider_clear` helpers.
    // They must validate through ProviderKind and surface the
    // canonical `as_str` literal so the resolver and the snapshot
    // observe identical strings.
    with_isolated_home("global-default-helpers", |_home| {
        // Initially absent: `provider_get` reports `null` so callers
        // can distinguish "unset" from "explicitly forgejo".
        let initial = config::provider_get().unwrap();
        assert!(
            initial.provider.is_none(),
            "fresh storage must report null default: {initial:?}"
        );

        // Set + get: every supported value must round-trip through
        // the helpers.
        for literal in [PROVIDER_FORGEJO, PROVIDER_REDMINE, PROVIDER_GITLAB] {
            let outcome = config::provider_set(literal).unwrap();
            assert_eq!(outcome.provider, Some(literal));
            let stored = config::provider_get().unwrap();
            assert_eq!(stored.provider, Some(literal));
        }

        // Clear: the first call returns `cleared: true` because a
        // row existed; the second call returns `cleared: false`
        // because the row is already absent.
        let cleared = config::provider_clear().unwrap();
        assert!(cleared.cleared, "first clear must remove the row");
        let cleared_again = config::provider_clear().unwrap();
        assert!(!cleared_again.cleared, "second clear must be a no-op");
        assert!(
            config::provider_get().unwrap().provider.is_none(),
            "clear must leave the default unset"
        );
    });
}

#[test]
fn config_provider_set_helper_rejects_unknown_value() {
    // Phase `global-provider-default`: invalid input must surface a
    // structured config error before any SQLite write happens. The
    // facade exposes `provider_set` directly so the test can probe
    // the helper without going through the CLI dispatcher (the CLI
    // path is covered separately by
    // `config_provider_set_rejects_unknown_value`).
    with_isolated_home("global-default-invalid", |_home| {
        let error = config::provider_set("wrong").unwrap_err();
        assert!(
            error.contains("invalid provider"),
            "error must name the offending value: {error}"
        );
        assert!(
            error.contains("wrong"),
            "error must echo the value: {error}"
        );

        let storage = Storage::open().unwrap();
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
    // A previously-persisted row that contains an unknown literal
    // (for example from a future build that has been downgraded)
    // must surface as a structured config error rather than being
    // echoed verbatim. The helper validates through `ProviderKind`
    // so the snapshot and the CLI both observe the same canonical
    // literal.
    with_isolated_home("global-default-stale", |_home| {
        let storage = Storage::open().unwrap();
        storage
            .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "wrong")
            .unwrap();

        let error = config::provider_get().unwrap_err();
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
    // Phase `global-provider-default`: the snapshot must surface
    // the machine-wide default through both the dedicated
    // `global_default_provider` field and the `global_settings`
    // entry. The literal must be one of the canonical
    // forgejo/redmine/gitlab strings; no secret values may leak
    // through the snapshot.
    with_isolated_home("show-global-default", |_home| {
        let _unset_default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "");

        // Persisted default via the helper exercises the same path
        // `config provider set` uses, so the snapshot must observe
        // the canonical literal.
        config::provider_set(PROVIDER_GITLAB).unwrap();

        let snapshot = config::show(None).unwrap();
        let text = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            snapshot["global_default_provider"].as_str(),
            Some(PROVIDER_GITLAB),
            "snapshot must surface the machine-wide default"
        );

        // The non-secret literal is also rendered inside
        // `global_settings` so callers iterating the canonical
        // list still observe the value.
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
        // Mirror secrets stay redacted regardless of the global
        // default value.
        assert!(
            !text.contains("mirror-bearer-secret"),
            "snapshot must never leak secret values: {text}"
        );

        // The unset case omits the top-level slot entirely so
        // absence means "not configured".
        config::provider_clear().unwrap();
        let unset = config::show(None).unwrap();
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
        // Ensure the new field name is at least named in the
        // rendered JSON so downstream tooling can switch on it.
        assert!(unset_text.contains("global_default_provider"));
    });
}

#[test]
fn import_env_persists_global_default_provider_and_validates_value() {
    // Phase `global-provider-default`: `config import-env` must
    // treat `PHASEGENT_DEFAULT_PROVIDER` as a global setting,
    // validate the value through `ProviderKind`, and report the
    // imported/skipped counts without echoing the literal.
    with_isolated_home("import-env-global-default", |_home| {
        let _unset_provider = EnvGuard::set("PHASEGENT_PROVIDER", "");
        let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_REDMINE);

        let outcome = config::import_env(Role::Executor).unwrap();
        let text = serde_json::to_string(&outcome).unwrap();
        assert!(
            !text.contains(PROVIDER_REDMINE),
            "import-env JSON must never echo the literal: {text}"
        );

        let global_names: Vec<&str> = outcome
            .global_settings
            .iter()
            .map(|entry| entry.name)
            .collect();
        assert!(
            global_names.contains(&"PHASEGENT_DEFAULT_PROVIDER"),
            "import-env must list the global default: {global_names:?}"
        );
        let default_entry = outcome
            .global_settings
            .iter()
            .find(|entry| entry.name == "PHASEGENT_DEFAULT_PROVIDER")
            .expect("global default entry");
        assert!(
            default_entry.imported,
            "valid value must be reported as imported"
        );
        assert!(
            !default_entry.secret,
            "global default is not a secret: {default_entry:?}"
        );

        let storage = Storage::open().unwrap();
        assert_eq!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .as_deref(),
            Some(PROVIDER_REDMINE)
        );
    });
}

#[test]
fn import_env_rejects_invalid_global_default_provider_value() {
    // Phase `global-provider-default`: a hostile or typo'd
    // `PHASEGENT_DEFAULT_PROVIDER` export must surface as a
    // structured config error before any SQLite write. The
    // validation runs through `ProviderKind::from_str` so the
    // resolver and the snapshot both observe the same canonical
    // strings.
    with_isolated_home("import-env-global-default-bad", |_home| {
        let _default = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", "wrong");

        let error = config::import_env(Role::Executor).unwrap_err();
        assert!(
            error.contains("PHASEGENT_DEFAULT_PROVIDER"),
            "error must name the offending env var: {error}"
        );
        assert!(
            error.contains("wrong"),
            "error must echo the offending value: {error}"
        );

        let storage = Storage::open().unwrap();
        assert!(
            storage
                .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
                .is_none(),
            "rejected value must not land in SQLite"
        );
    });
}
