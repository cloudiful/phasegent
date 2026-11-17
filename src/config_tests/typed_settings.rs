//! Focused unit tests for the typed configuration tree, its canonical
//! paths, and the CST edits that write it.

use crate::infra::config_file::setting_path::{self, GLOBAL_SETTINGS};
use crate::infra::config_file::{ConfigFile, SettingValue};

#[test]
fn decoded_nulls_and_invalid_global_domains_are_refused() {
    use crate::infra::config_file::ConfigDocument;
    for json in [
        r#"{"version":2,"provider":null}"#,
        r#"{"version":2,"redmine":null}"#,
        r#"{"version":2,"notifications":{"enabled":null}}"#,
        r#"{"version":2,"notifications":{"email":{"smtp":{"port":null}}}}"#,
        r#"{"version":2,"notifications":{"email":{"smtp":{"port":0}}}}"#,
        r#"{"version":2,"notifications":{"email":{"to":["ok", " "]}}}"#,
        r#"{"version":2,"notifications":{"email":{"to":[]}}}"#,
        r#"{"version":2,"notifications":{"channel":"EMAIL"}}"#,
        r#"{"version":2,"roles":{"executor":{"provider":null}}}"#,
    ] {
        assert!(ConfigDocument::parse(json).is_err(), "accepted {json}");
    }
    assert!(ConfigDocument::parse(r#"{"version":2}"#).is_ok());
    let mut model =
        ConfigDocument::parse(r#"{"version":2,"notifications":{"email":{"smtp":{"port":1}}}}"#)
            .unwrap()
            .model()
            .clone();
    model
        .notifications
        .as_mut()
        .unwrap()
        .email
        .as_mut()
        .unwrap()
        .smtp
        .as_mut()
        .unwrap()
        .port = Some(0);
    assert!(model.validate().is_err());
}

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

#[test]
fn every_canonical_path_is_unique_and_maps_to_one_legacy_name() {
    let mut paths = std::collections::HashSet::new();
    let mut legacy = std::collections::HashSet::new();
    for setting in GLOBAL_SETTINGS {
        assert!(
            paths.insert(setting.dotted),
            "the path {} is used twice",
            setting.dotted
        );
        assert!(
            legacy.insert(setting.legacy_name),
            "the legacy name {} is used twice",
            setting.legacy_name
        );
        assert_eq!(
            setting_path::global_setting_by_legacy_name(setting.legacy_name)
                .map(|candidate| candidate.dotted),
            Some(setting.dotted)
        );
        assert_eq!(
            setting_path::setting_segments(setting.dotted),
            Some(setting.segments)
        );
        assert_eq!(
            setting.segments.last().copied(),
            setting.dotted.split('.').next_back()
        );
    }
    // The dotted path is the last segment only for a top-level setting; a
    // nested one carries its full path in the segments.
    assert_eq!(
        setting_path::setting_segments("redmine.apiBase"),
        Some(&["redmine", "apiBase"][..])
    );
    assert_eq!(
        setting_path::setting_segments("redmine.gitMirror.apiKey"),
        Some(&["redmine", "gitMirror", "apiKey"][..])
    );
}

#[test]
fn the_removed_index_backend_has_no_destination() {
    assert!(setting_path::global_setting_by_legacy_name("PHASEGENT_INDEX_BACKEND").is_none());
    assert!(setting_path::global_setting("index.backend").is_none());
    assert!(setting_path::global_setting("index.postgres.url").is_some());
}

#[test]
fn a_typed_value_keeps_its_type_in_the_model() {
    let mut file = ConfigFile::new();
    file.set_setting("notifications.enabled", SettingValue::Bool(true))
        .unwrap();
    file.set_setting("notifications.email.smtp.port", SettingValue::Port(587))
        .unwrap();
    file.set_setting(
        "notifications.email.to",
        SettingValue::List(vec!["ops@example.com".to_owned()]),
    )
    .unwrap();
    file.set_setting("provider", text("forgejo")).unwrap();

    assert_eq!(
        file.setting("notifications.enabled"),
        Some(SettingValue::Bool(true))
    );
    assert_eq!(
        file.setting("notifications.email.smtp.port"),
        Some(SettingValue::Port(587))
    );
    assert_eq!(
        file.setting("notifications.email.to"),
        Some(SettingValue::List(vec!["ops@example.com".to_owned()]))
    );
    assert_eq!(file.setting("provider"), Some(text("forgejo")));
    assert_eq!(file.provider(crate::policy::Role::Executor), None);
}

#[test]
fn a_value_of_the_wrong_type_is_refused() {
    let mut file = ConfigFile::new();
    assert!(
        file.set_setting("notifications.enabled", text("true"))
            .is_err(),
        "a boolean field must refuse a text value"
    );
    assert!(
        file.set_setting("notifications.email.smtp.port", SettingValue::Port(0))
            .is_err(),
        "a port must be greater than zero"
    );
    assert!(
        file.set_setting("provider", text("bitbucket")).is_err(),
        "an unknown literal must be refused"
    );
    assert!(
        file.set_setting("notifications.email.to", SettingValue::List(Vec::new()))
            .is_err(),
        "an empty list must be refused"
    );
    assert!(
        file.set_setting("unknown.path", text("x")).is_err(),
        "an unknown path must be refused"
    );
}

#[test]
fn clearing_a_leaf_keeps_its_siblings() {
    let mut file = ConfigFile::new();
    file.set_setting("notifications.ntfy.token", text("secret"))
        .unwrap();
    file.set_setting("notifications.ntfy.topic", text("phasegent"))
        .unwrap();
    assert!(file.clear_setting("notifications.ntfy.token"));
    assert_eq!(file.setting("notifications.ntfy.token"), None);
    assert_eq!(
        file.setting("notifications.ntfy.topic"),
        Some(text("phasegent"))
    );
    assert!(
        !file.clear_setting("notifications.ntfy.token"),
        "a second clear changes nothing"
    );
}

#[test]
fn a_version_one_file_is_refused_with_an_upgrade_instruction() {
    let mut file = ConfigFile::new();
    file.version = 1;
    let error = file.validate().expect_err("version 1 must be refused");
    assert!(error.contains("schema version 1"), "got: {error}");
    assert!(error.contains("admin config upgrade"), "got: {error}");
}

#[test]
fn the_flat_setting_names_resolve_to_their_dotted_paths() {
    for (alias, expected) in [
        ("provider", "provider"),
        ("PHASEGENT_DEFAULT_PROVIDER", "provider"),
        ("default-provider", "provider"),
        ("redmine.apiBase", "redmine.apiBase"),
        ("PHASEGENT_REDMINE_API_BASE", "redmine.apiBase"),
        ("redmine-api-base", "redmine.apiBase"),
        (
            "notifications.email.smtp.port",
            "notifications.email.smtp.port",
        ),
        ("notify-email-smtp-port", "notifications.email.smtp.port"),
        ("index.postgres.url", "index.postgres.url"),
        ("index-pg-url", "index.postgres.url"),
        (
            "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
            "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
        ),
    ] {
        assert_eq!(
            crate::config_write::canonical_setting_name(alias),
            Some(expected),
            "alias {alias}"
        );
    }
    assert_eq!(
        crate::config_write::canonical_setting_name("index-backend"),
        None,
        "the removed index backend selector has no alias"
    );
}

#[test]
fn the_secret_settings_keep_their_write_only_policy() {
    for path in [
        "redmine.gitMirror.apiKey",
        "index.postgres.url",
        "notifications.ntfy.token",
        "notifications.webhook.token",
        "notifications.dingtalk.secret",
        "notifications.email.password",
    ] {
        assert!(
            crate::config_write::is_secret_setting(path),
            "{path} must stay write-only"
        );
        assert!(crate::config_write::is_global_setting(path));
    }
    assert!(!crate::config_write::is_secret_setting("redmine.apiBase"));
    assert!(crate::config_write::allows_direct_secret_value(
        "notifications.email.password"
    ));
}

#[test]
fn the_provider_alias_keeps_its_scope() {
    // The dotted path names the machine-wide default provider.
    assert_eq!(
        crate::config_write::canonical_setting_name("provider"),
        Some("provider")
    );
    assert!(crate::config_write::is_global_setting("provider"));
    // The historical environment name of the role-scoped provider keeps its
    // scope, so an existing script never changes what it writes.
    assert_eq!(
        crate::config_write::canonical_setting_name("PHASEGENT_PROVIDER"),
        Some("PHASEGENT_PROVIDER")
    );
    assert!(crate::config_write::is_role_scoped_setting(
        "PHASEGENT_PROVIDER"
    ));
    assert_eq!(
        crate::config_write::canonical_setting_name("phasegent_provider"),
        Some("PHASEGENT_PROVIDER")
    );
    // The machine-wide default provider is also reachable under its
    // historical environment name.
    assert_eq!(
        crate::config_write::canonical_setting_name("PHASEGENT_DEFAULT_PROVIDER"),
        Some("provider")
    );
}

#[test]
fn the_config_command_accepts_a_dotted_path() {
    use crate::command::{self, Command};
    let args = [
        "admin",
        "config",
        "set",
        "notifications.email.smtp.port",
        "587",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, None)
        .expect("a dotted path must parse as a machine-wide setting");
    match invocation.command {
        Command::ConfigSet { setting, value, .. } => {
            assert_eq!(setting, "notifications.email.smtp.port");
            assert_eq!(value.as_deref(), Some("587"));
        }
        other => panic!("expected ConfigSet, got {other:?}"),
    }
    let args = ["admin", "config", "clear", "notifications.email.smtp.port"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let invocation = command::parse_with_role_env(&args, None)
        .expect("a dotted path must parse for a clear as well");
    match invocation.command {
        Command::ConfigClear { setting, .. } => {
            assert_eq!(setting, "notifications.email.smtp.port");
        }
        other => panic!("expected ConfigClear, got {other:?}"),
    }
}
