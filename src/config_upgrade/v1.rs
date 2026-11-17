//! The schema-v1 configuration decoder.
//!
//! This module exists only inside the explicit upgrader. The runtime never
//! reads a version-1 document: the version-2 model refuses one with an
//! actionable upgrade instruction, and the version-1 decoder below is never
//! reached from a read or a write path.
//!
//! The decoder is strict. It accepts the exact version-1 shape - a flat
//! `settings` map of `PHASEGENT_*` names plus the per-role entries - and
//! refuses everything else: an unknown field, a duplicate key, a value that
//! is not a string, an empty value, and a value the version-1 validators
//! rejected. A refusal happens before anything is written, so a file that
//! this build cannot upgrade keeps its bytes.

use crate::infra::config_file::RoleConfig;
use crate::infra::config_file::{check_duplicate_keys, parse_root};
use crate::notifications::config::NotifyChannel;
use crate::providers::config::ProviderKind;
use jsonc_parser::cst::CstObjectProp;
use std::collections::BTreeMap;

/// The deprecated version-1 index-backend selector.
///
/// The selector was always inert: the presence of the PostgreSQL URL selects
/// the backend. It has no destination in the version-2 tree, so the upgrade
/// drops it on purpose and reports the removal.
pub const LEGACY_INDEX_BACKEND: &str = "PHASEGENT_INDEX_BACKEND";

/// The decoded version-1 document.
/// One version-1 setting: the flat name, the persisted value, and the
/// comments the operator wrote next to it.
#[derive(Clone, Debug, PartialEq)]
pub struct V1Setting {
    /// The flat `PHASEGENT_*` name.
    pub name: String,
    /// The persisted value, exactly as the file holds it.
    pub value: String,
    /// The comment lines that preceded the setting in the file.
    pub comments: Vec<String>,
}

/// The decoded version-1 document.
#[derive(Clone, Debug, PartialEq)]
pub struct V1Document {
    /// The settings in file order.
    pub settings: Vec<V1Setting>,
    /// The per-role entries.
    pub roles: BTreeMap<String, RoleConfig>,
}

/// Parse a version-1 configuration text into its document.
///
/// The text is read with the same strict JSONC profile the runtime uses, so
/// comments and trailing commas are accepted and every other JSON5
/// extension is refused.
pub fn parse_document(text: &str) -> Result<V1Document, String> {
    let root = parse_root(text)?;
    let Some(value) = root.value() else {
        return Err("the configuration file must contain a top-level object".to_owned());
    };
    if value.as_object().is_none() {
        return Err("the configuration file must contain a top-level object".to_owned());
    }
    check_duplicate_keys(&value)?;
    let object = value.as_object().expect("the value is an object");
    let mut version: Option<u32> = None;
    let mut settings: Option<Vec<V1Setting>> = None;
    let mut roles: Option<BTreeMap<String, RoleConfig>> = None;
    for property in object.properties() {
        let name = property
            .decoded_name()
            .ok_or_else(|| "the configuration file holds an invalid property name".to_owned())?;
        match name.as_str() {
            "version" => {
                let node = property
                    .value()
                    .ok_or_else(|| "the configuration file has no version".to_owned())?;
                let raw = node.to_string();
                let parsed = raw.trim().parse::<u32>().map_err(|_| {
                    "the configuration file has no usable schema version".to_owned()
                })?;
                version = Some(parsed);
            }
            "settings" => {
                settings = Some(read_settings(&property)?);
            }
            "roles" => {
                roles = Some(read_roles(&property)?);
            }
            _ => {
                // The field name comes from the file and is never echoed
                // back: a name can be an accidentally pasted secret.
                return Err(
                    "the configuration file holds a field the version-1 schema does not define; \
                     refusing to upgrade it"
                        .to_owned(),
                );
            }
        }
    }
    let version = version.ok_or_else(|| {
        "the configuration file does not declare a schema version; refusing to upgrade it"
            .to_owned()
    })?;
    if version != 1 {
        return Err(format!(
            "unsupported configuration version {version}; the upgrader reads version 1 only"
        ));
    }
    let settings = settings.unwrap_or_default();
    for setting in &settings {
        validate_setting(&setting.name, &setting.value)?;
    }
    Ok(V1Document {
        settings,
        roles: roles.unwrap_or_default(),
    })
}

/// Read the flat settings map of a version-1 document.
fn read_settings(property: &CstObjectProp) -> Result<Vec<V1Setting>, String> {
    let Some(node) = property.value() else {
        return Err("the version-1 settings map is empty".to_owned());
    };
    let Some(object) = node.as_object() else {
        return Err("the version-1 settings map must be an object".to_owned());
    };
    let mut settings = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for child in object.children() {
        if child.is_comment() {
            pending.push(child.to_string());
            continue;
        }
        let Some(property) = child.as_object_prop() else {
            continue;
        };
        let name = property
            .decoded_name()
            .ok_or_else(|| "the version-1 settings map holds an invalid name".to_owned())?;
        let Some(value_node) = property.value() else {
            return Err("the version-1 settings map holds a value-less entry".to_owned());
        };
        let Some(literal) = value_node.as_string_lit() else {
            return Err(
                "the version-1 settings map holds a value that is not a string; refusing to \
                 upgrade it"
                    .to_owned(),
            );
        };
        let value = literal
            .decoded_value()
            .map_err(|_| "the version-1 settings map holds an unreadable value".to_owned())?;
        settings.push(V1Setting {
            name,
            value,
            comments: std::mem::take(&mut pending),
        });
    }
    Ok(settings)
}

/// Read the per-role entries of a version-1 document.
fn read_roles(property: &CstObjectProp) -> Result<BTreeMap<String, RoleConfig>, String> {
    let Some(node) = property.value() else {
        return Err("the version-1 roles map is empty".to_owned());
    };
    let Some(object) = node.as_object() else {
        return Err("the version-1 roles map must be an object".to_owned());
    };
    let mut roles = BTreeMap::new();
    for property in object.properties() {
        let name = property
            .decoded_name()
            .ok_or_else(|| "the version-1 roles map holds an invalid role name".to_owned())?;
        let Some(value_node) = property.value() else {
            return Err("the version-1 roles map holds a role without a value".to_owned());
        };
        let decoded = value_node
            .to_serde_value()
            .ok_or_else(|| "the version-1 roles map holds an unreadable role".to_owned())?;
        let role: RoleConfig = serde_json::from_value(decoded).map_err(|_| {
            "the version-1 roles map holds a role the configuration model cannot represent; \
             refusing to upgrade it"
                .to_owned()
        })?;
        roles.insert(name, role);
    }
    Ok(roles)
}

/// Validate one version-1 setting with the version-1 rules.
///
/// The rules are the ones the version-1 writer and reader applied, so a file
/// that this build refuses is a file the version-1 build refused too. The
/// value never appears in an error: a persisted value can be a secret.
pub fn validate_setting(name: &str, value: &str) -> Result<(), String> {
    if !is_known_v1_setting(name) {
        // The setting name comes from the file and is never echoed back: a
        // name can be an accidentally pasted secret.
        return Err(
            "the configuration file holds a setting this build does not know; refusing to \
             upgrade it"
                .to_owned(),
        );
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(
            "the configuration file holds an empty value for a setting; refusing to upgrade it"
                .to_owned(),
        );
    }
    match name {
        LEGACY_INDEX_BACKEND => {
            let lower = trimmed.to_ascii_lowercase();
            if lower != "sqlite" && lower != "postgres" {
                return Err(
                    "the configuration file holds an invalid value for a setting; refusing to \
                     upgrade it"
                        .to_owned(),
                );
            }
        }
        "PHASEGENT_DEFAULT_PROVIDER" => {
            trimmed.parse::<ProviderKind>().map_err(|_| {
                "the configuration file holds an invalid value for a setting; refusing to \
                 upgrade it"
                    .to_owned()
            })?;
        }
        "PHASEGENT_NOTIFY_ENABLED" => {
            if crate::config_write::parse_bool_literal(trimmed).is_none() {
                return Err(
                    "the configuration file holds an invalid value for a setting; refusing to \
                     upgrade it"
                        .to_owned(),
                );
            }
        }
        "PHASEGENT_NOTIFY_CHANNEL" => {
            NotifyChannel::parse(trimmed).map_err(|_| {
                "the configuration file holds an invalid value for a setting; refusing to \
                 upgrade it"
                    .to_owned()
            })?;
        }
        "PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE" => {
            let lower = trimmed.to_ascii_lowercase();
            if lower != "text" && lower != "markdown" {
                return Err(
                    "the configuration file holds an invalid value for a setting; refusing to \
                     upgrade it"
                        .to_owned(),
                );
            }
        }
        "PHASEGENT_NOTIFY_EMAIL_SMTP_PORT" => {
            let port = trimmed.parse::<u16>().map_err(|_| {
                "the configuration file holds an invalid value for a setting; refusing to \
                 upgrade it"
                    .to_owned()
            })?;
            if port == 0 {
                return Err(
                    "the configuration file holds an invalid value for a setting; refusing to \
                     upgrade it"
                        .to_owned(),
                );
            }
        }
        "PHASEGENT_NOTIFY_EMAIL_TLS" => {
            let lower = trimmed.to_ascii_lowercase();
            if !matches!(lower.as_str(), "auto" | "implicit" | "starttls" | "plain") {
                return Err(
                    "the configuration file holds an invalid value for a setting; refusing to \
                     upgrade it"
                        .to_owned(),
                );
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether the name is a setting the version-1 schema defined.
///
/// The list is the current canonical set plus the deprecated index-backend
/// selector, which the version-1 schema accepted and the version-2 schema
/// dropped on purpose.
fn is_known_v1_setting(name: &str) -> bool {
    name == LEGACY_INDEX_BACKEND
        || crate::infra::storage_schema::GLOBAL_SETTING_NAMES.contains(&name)
}
