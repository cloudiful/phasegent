use super::support::*;
use super::*;

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
        assert_eq!(
            global.len(),
            crate::infra::storage_schema::GLOBAL_SETTING_NAMES.len()
        );
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
