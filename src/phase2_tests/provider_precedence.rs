use super::*;

#[test]
fn resolve_kind_prefers_role_scoped_gitlab_when_env_var_unset() {
    // Phase-1 GitLab foundation: when `--provider gitlab` reaches
    // `resolve_kind` and the `PHASEGENT_PROVIDER` env var is unset
    // (the test isolates `HOME` and clears the var), the resolver
    // falls back to the role_config.provider column. A pre-populated
    // row must be consulted without leaking the role into another
    // provider branch.
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let home = crate::test_scratch::root().join(format!(
        "phasegent-resolve-kind-gitlab-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _db_path_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        home.join(crate::infra::storage::DB_FILENAME)
            .to_string_lossy()
            .as_ref(),
    );
    let previous_provider = std::env::var_os("PHASEGENT_PROVIDER");
    // SAFETY:: Serialised by `lock_workflow_tests`. The Drop guard on
    // `previous_provider` reinstates the host value when the test
    // unwinds even if a panic happens mid-test.
    struct ProviderGuard(Option<std::ffi::OsString>);
    impl Drop for ProviderGuard {
        fn drop(&mut self) {
            let previous = self.0.take();
            // SAFETY:: Symmetric with the unsafe block below.
            unsafe {
                match previous {
                    Some(value) => std::env::set_var("PHASEGENT_PROVIDER", value),
                    None => std::env::remove_var("PHASEGENT_PROVIDER"),
                }
            }
        }
    }
    unsafe {
        std::env::remove_var("PHASEGENT_PROVIDER");
    }
    let _provider_guard = ProviderGuard(previous_provider);

    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: Some("gitlab".to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();

    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(resolved, ProviderKind::Gitlab);

    // A stale Redmine row for the same role must not win when
    // resolve_kind is called with an explicit gitlab.
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: Some("redmine".to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();
    let explicit =
        crate::providers::config::resolve_kind(Role::Orchestrator, Some(ProviderKind::Gitlab))
            .unwrap();
    assert_eq!(explicit, ProviderKind::Gitlab);

    let _ = fs::remove_dir_all(home);
}

/// RAII guard that removes every `PHASEGENT_PROVIDER` /
/// `PHASEGENT_DEFAULT_PROVIDER` variant for the lifetime of the
/// test and reinstates the host value on Drop. The new precedence
/// levels added by phase `global-provider-default` all read
/// environment variables, so the resolver tests need to neutralise
/// the host shell's environment before exercising the resolver
/// and restore it on exit.
#[allow(non_snake_case)]
struct DefaultProviderEnvGuard {
    _provider: Option<std::ffi::OsString>,
    _default: Option<std::ffi::OsString>,
}

impl DefaultProviderEnvGuard {
    fn neutralise() -> Self {
        let provider = std::env::var_os("PHASEGENT_PROVIDER");
        let default = std::env::var_os("PHASEGENT_DEFAULT_PROVIDER");
        // SAFETY: serialised by `lock_workflow_tests`; the Drop
        // guard reinstates the host value when the test unwinds
        // even if a panic happens mid-test.
        unsafe {
            std::env::remove_var("PHASEGENT_PROVIDER");
            std::env::remove_var("PHASEGENT_DEFAULT_PROVIDER");
        }
        Self {
            _provider: provider,
            _default: default,
        }
    }
}

impl Drop for DefaultProviderEnvGuard {
    fn drop(&mut self) {
        let provider = self._provider.take();
        let default = self._default.take();
        // SAFETY: symmetric with the unsafe block above; the lock
        // guard from `lock_workflow_tests` is still held when the
        // test stack unwinds.
        unsafe {
            match provider {
                Some(value) => std::env::set_var("PHASEGENT_PROVIDER", value),
                None => std::env::remove_var("PHASEGENT_PROVIDER"),
            }
            match default {
                Some(value) => std::env::set_var("PHASEGENT_DEFAULT_PROVIDER", value),
                None => std::env::remove_var("PHASEGENT_DEFAULT_PROVIDER"),
            }
        }
    }
}

#[test]
fn resolve_kind_honours_documented_provider_precedence_chain() {
    // Phase `global-provider-default`: the resolver must consult
    // every documented precedence level in order:
    //   1. explicit --provider argument
    //   2. PHASEGENT_PROVIDER environment variable
    //   3. PHASEGENT_DEFAULT_PROVIDER environment variable
    //   4. persisted PHASEGENT_DEFAULT_PROVIDER row in SQLite
    //   5. role-scoped role_config.provider
    //   6. forgejo fallback
    // The resolver is read-only: each test fully resets the
    // environment and storage so the order of precedence is the
    // only variable under inspection.
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    use crate::infra::storage::{PROVIDER_GITLAB, PROVIDER_REDMINE};

    let _lock = lock_workflow_tests();
    let _provider_env = DefaultProviderEnvGuard::neutralise();
    let home = crate::test_scratch::root().join(format!(
        "phasegent-resolve-precedence-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _db_path_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        home.join(crate::infra::storage::DB_FILENAME)
            .to_string_lossy()
            .as_ref(),
    );

    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: Some(PROVIDER_GITLAB.to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();

    // 6. Forgejo fallback when nothing else is configured.
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "")
        .unwrap();
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: None,
                api_base: None,
                repository: None,
            },
        )
        .unwrap();
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Forgejo,
        "empty persisted default + empty role-scoped provider must fall back to forgejo"
    );

    // 5. Role-scoped provider beats the forgejo fallback.
    storage
        .save_role_config(
            Role::Orchestrator,
            &auth::StoredConfig {
                provider: Some(PROVIDER_REDMINE.to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Redmine,
        "role-scoped provider must win over the forgejo fallback"
    );

    // 4. Persisted global default beats the role-scoped provider.
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_GITLAB)
        .unwrap();
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Gitlab,
        "persisted PHASEGENT_DEFAULT_PROVIDER must win over role-scoped provider"
    );

    // 3. Env default beats the persisted default.
    let _default_env = EnvGuard::set("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_REDMINE);
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Redmine,
        "PHASEGENT_DEFAULT_PROVIDER env var must win over persisted default"
    );

    // 2. PHASEGENT_PROVIDER env var beats the default env var.
    let _provider_env = EnvGuard::set("PHASEGENT_PROVIDER", PROVIDER_GITLAB);
    let resolved = crate::providers::config::resolve_kind(Role::Orchestrator, None).unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Gitlab,
        "PHASEGENT_PROVIDER must win over PHASEGENT_DEFAULT_PROVIDER"
    );

    // 1. Explicit --provider beats every environment / storage
    // value. This documents the contract that `--provider` is the
    // per-command override.
    let resolved =
        crate::providers::config::resolve_kind(Role::Orchestrator, Some(ProviderKind::Redmine))
            .unwrap();
    assert_eq!(
        resolved,
        ProviderKind::Redmine,
        "explicit --provider must beat every env / storage value"
    );

    // Resolver must never persist anything: the persisted default
    // is exactly what the test seeded, no surprises.
    assert_eq!(
        storage
            .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .unwrap()
            .as_deref(),
        Some(PROVIDER_GITLAB)
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn resolve_kind_rejects_invalid_persisted_global_default() {
    // Phase `global-provider-default`: a stale SQLite row that
    // contains an unknown literal must surface as a structured
    // config error rather than silently overriding the resolver.
    // The validator is the same `ProviderKind::from_str` that the
    // helper / snapshot / CLI layer use, so the contract is
    // uniform end to end.
    use crate::infra::storage::PROVIDER_REDMINE;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let _provider_env = DefaultProviderEnvGuard::neutralise();
    let home = crate::test_scratch::root().join(format!(
        "phasegent-resolve-stale-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _db_path_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        home.join(crate::infra::storage::DB_FILENAME)
            .to_string_lossy()
            .as_ref(),
    );

    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", "wrong")
        .unwrap();
    // The role-scoped row points at a valid value so the error
    // surfaces from the persisted-default level rather than the
    // role-scoped level.
    storage
        .save_role_config(
            Role::Executor,
            &auth::StoredConfig {
                provider: Some(PROVIDER_REDMINE.to_owned()),
                api_base: None,
                repository: None,
            },
        )
        .unwrap();

    let error = crate::providers::config::resolve_kind(Role::Executor, None).unwrap_err();
    let json = error.json();
    assert_eq!(json["kind"], "config");
    assert!(
        json["message"]
            .as_str()
            .unwrap_or_default()
            .contains("wrong"),
        "error must echo the offending value: {error:?}"
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn resolve_kind_does_not_persist_anything() {
    // Regression guard for the "ordinary commands must not persist"
    // contract: phase `global-provider-default` adds a SQLite read
    // to `resolve_kind`, so the test must observe the empty
    // database after the resolver runs. The seeded values are
    // written by the test, not by the resolver.
    use crate::infra::storage::PROVIDER_REDMINE;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let _provider_env = DefaultProviderEnvGuard::neutralise();
    let home = crate::test_scratch::root().join(format!(
        "phasegent-resolve-readonly-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _db_path_guard = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        home.join(crate::infra::storage::DB_FILENAME)
            .to_string_lossy()
            .as_ref(),
    );

    let storage = Storage::open_at(&home.join(crate::infra::storage::DB_FILENAME)).unwrap();
    storage
        .save_global_setting("PHASEGENT_DEFAULT_PROVIDER", PROVIDER_REDMINE)
        .unwrap();

    let resolved = crate::providers::config::resolve_kind(Role::Executor, None).unwrap();
    assert_eq!(resolved, ProviderKind::Redmine);

    // The persisted default must still be exactly what the test
    // wrote; the resolver must never silently mutate it.
    assert_eq!(
        storage
            .load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .unwrap()
            .as_deref(),
        Some(PROVIDER_REDMINE)
    );
    assert!(
        storage.load_role_config(Role::Executor).unwrap().is_none(),
        "resolver must never write the role-scoped row"
    );

    let _ = fs::remove_dir_all(home);
}
