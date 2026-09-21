use super::support::*;
use super::*;

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
