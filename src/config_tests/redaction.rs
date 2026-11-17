//! Redaction tests for the configuration surfaces.
//!
//! Property names, role names, credential providers, and parser
//! diagnostics all come from untrusted files. A name can be an
//! accidentally pasted secret, and a parser diagnostic can quote the
//! source line, so none of them may appear in an error message.

use crate::infra::config_file::{ConfigDocument, ConfigFile};
use crate::infra::config_file::{LegacyConfigRows, LegacyRoleConfigRow};
use serde_json::json;

#[test]
fn an_unknown_setting_name_is_never_echoed() {
    let document = json!({
        "version": 1,
        "settings": { "SYNTHETIC_SECRET_KEY_ABC": "value" },
        "roles": {}
    });
    let error = ConfigDocument::parse(&document.to_string())
        .expect_err("an unknown setting must be refused");
    assert!(
        !error.contains("SYNTHETIC_SECRET_KEY_ABC"),
        "the untrusted name leaked: {error}"
    );
}

#[test]
fn an_unknown_role_name_is_never_echoed() {
    let document = json!({
        "version": 1,
        "settings": {},
        "roles": { "SYNTHETIC_SECRET_ROLE": { "provider": "forgejo" } }
    });
    let error =
        ConfigDocument::parse(&document.to_string()).expect_err("an unknown role must be refused");
    assert!(
        !error.contains("SYNTHETIC_SECRET_ROLE"),
        "the untrusted name leaked: {error}"
    );
}

#[test]
fn an_unknown_credential_provider_is_never_echoed() {
    let document = json!({
        "version": 1,
        "settings": {},
        "roles": {
            "executor": {
                "credentials": { "SYNTHETIC_SECRET_PROVIDER": { "value": "secret" } }
            }
        }
    });
    let error = ConfigDocument::parse(&document.to_string())
        .expect_err("an unknown credential provider must be refused");
    assert!(
        !error.contains("SYNTHETIC_SECRET_PROVIDER"),
        "the untrusted name leaked: {error}"
    );
}

#[test]
fn an_unknown_legacy_role_is_never_echoed() {
    let rows = LegacyConfigRows {
        role_configs: vec![LegacyRoleConfigRow {
            role: "SYNTHETIC_SECRET_ROLE".to_owned(),
            provider: Some("forgejo".to_owned()),
            api_base: None,
            repository: None,
        }],
        ..LegacyConfigRows::default()
    };
    let error = ConfigFile::from_legacy(rows).expect_err("an unknown role must be refused");
    assert!(
        !error.contains("SYNTHETIC_SECRET_ROLE"),
        "the untrusted name leaked: {error}"
    );
}

#[test]
fn a_malformed_toml_overlay_never_quotes_the_source() {
    let error = crate::config_migrate::toml::parse_overlay(
        "default_provider = \"SYNTHETIC_SECRET_VALUE\nbroken = \n",
        "overlay.toml",
    )
    .expect_err("a malformed overlay must be refused");
    assert!(
        !error.contains("SYNTHETIC_SECRET_VALUE"),
        "the source leaked: {error}"
    );
    assert!(
        error.contains("invalid TOML syntax at line"),
        "the fixed category is missing: {error}"
    );
}

#[test]
fn an_unknown_toml_field_is_refused_without_its_name() {
    let error = crate::config_migrate::toml::parse_overlay(
        "SYNTHETIC_UNKNOWN_FIELD = \"value\"\n",
        "overlay.toml",
    )
    .expect_err("an unknown field must be refused");
    assert!(
        !error.contains("SYNTHETIC_UNKNOWN_FIELD"),
        "the untrusted name leaked: {error}"
    );
    assert!(error.contains("unknown field"), "got: {error}");
}

#[test]
fn an_invalid_toml_value_is_refused_without_its_value() {
    let error = crate::config_migrate::toml::parse_overlay(
        "default_provider = \"SYNTHETIC_SECRET_VALUE\"\n",
        "overlay.toml",
    )
    .expect_err("an invalid value must be refused");
    assert!(
        !error.contains("SYNTHETIC_SECRET_VALUE"),
        "the untrusted value leaked: {error}"
    );
}

#[test]
fn an_unknown_toml_role_and_role_field_are_refused_without_their_names() {
    let error = crate::config_migrate::toml::parse_overlay(
        "[roles.SYNTHETIC_SECRET_ROLE]\nprovider = \"redmine\"\n",
        "overlay.toml",
    )
    .expect_err("an unknown role must be refused");
    assert!(
        !error.contains("SYNTHETIC_SECRET_ROLE"),
        "the untrusted name leaked: {error}"
    );

    let error = crate::config_migrate::toml::parse_overlay(
        "[roles.executor]\nSYNTHETIC_UNKNOWN_FIELD = \"value\"\n",
        "overlay.toml",
    )
    .expect_err("an unknown role field must be refused");
    assert!(
        !error.contains("SYNTHETIC_UNKNOWN_FIELD"),
        "the untrusted name leaked: {error}"
    );
}
