//! Explicit `config set` / `config clear` persistence helpers.
//!
//! The module owns the canonical setting registry, validation, and
//! storage persistence for the explicit CLI configuration surface that
//! replaced `config import-env`. Environment variables remain
//! runtime-only overrides in `crate::auth` and `crate::providers::config`;
//! this module only touches the JSONC configuration file.
//!
//! A setting is named by its path inside the typed configuration tree
//! (`redmine.apiBase`, `notifications.email.smtp.port`). The historical
//! flat names (`PHASEGENT_REDMINE_API_BASE`) and their kebab-case forms
//! stay accepted as aliases so existing scripts keep working. The file
//! itself never stores a flat name.
//!
//! Secret handling: the Redmine git-mirror key and the other write-only
//! values are never accepted as a positional or option value. The CLI
//! dispatch rejects direct values before this module persists anything;
//! the module itself never echoes secret values in errors or JSON.
//!
//! Global settings are machine-wide and usable without a role;
//! role-scoped settings require a role and operate on the per-role
//! entry in the configuration file.

mod clear;
mod common;
mod set;

pub use clear::clear_setting;
pub use set::dispatch_set;

use crate::infra::config_file::setting_path::{
    GLOBAL_SETTINGS, GlobalSetting, ROLE_SCOPED_SETTINGS,
};
use serde::Serialize;

/// Resolve a user-supplied setting name to its canonical form.
///
/// The canonical form of a machine-wide setting is its path inside the
/// typed configuration tree. The canonical form of a role-scoped setting
/// is its flat name, because it has no machine-wide path. Matching is
/// case-insensitive and accepts the dotted path, the historical
/// `PHASEGENT_*` name, and the concise kebab-case aliases.
pub fn canonical_setting_name(input: &str) -> Option<&'static str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    for setting in GLOBAL_SETTINGS {
        if trimmed == setting.dotted || trimmed == setting.legacy_name {
            return Some(setting.dotted);
        }
    }
    for canonical in ROLE_SCOPED_SETTINGS {
        if trimmed == *canonical {
            return Some(canonical);
        }
    }
    let lowered = trimmed.to_ascii_lowercase();
    for setting in GLOBAL_SETTINGS {
        if aliases(setting).contains(&lowered) {
            return Some(setting.dotted);
        }
    }
    for canonical in ROLE_SCOPED_SETTINGS {
        let lowered_canonical = canonical.to_ascii_lowercase();
        let without_prefix = lowered_canonical
            .strip_prefix("phasegent_")
            .unwrap_or(lowered_canonical.as_str());
        if lowered == lowered_canonical
            || lowered == without_prefix
            || lowered == without_prefix.replace('_', "-")
        {
            return Some(canonical);
        }
    }
    None
}

/// The accepted aliases of one machine-wide setting.
fn aliases(setting: &GlobalSetting) -> Vec<String> {
    let legacy = setting.legacy_name.to_ascii_lowercase();
    let without_prefix = legacy
        .strip_prefix("phasegent_")
        .unwrap_or(legacy.as_str())
        .to_owned();
    vec![
        setting.dotted.to_ascii_lowercase(),
        setting.dotted.replace('.', "-").to_ascii_lowercase(),
        legacy.clone(),
        without_prefix.clone(),
        without_prefix.replace('_', "-"),
    ]
}

pub fn is_global_setting(name: &str) -> bool {
    GLOBAL_SETTINGS.iter().any(|setting| setting.dotted == name)
}

pub fn is_secret_setting(name: &str) -> bool {
    GLOBAL_SETTINGS
        .iter()
        .any(|setting| setting.dotted == name && setting.is_secret())
}

pub fn is_role_scoped_setting(name: &str) -> bool {
    !is_global_setting(name)
}

/// Secret settings whose value may be supplied directly on the command
/// line. Only the SMTP password is listed: Cloudflare Email Sending and
/// similar providers hand out an API token that operators paste as the
/// password, so a direct value is the supported flow. Every other secret
/// keeps the `--stdin`/prompt-only source policy.
pub fn allows_direct_secret_value(name: &str) -> bool {
    matches!(
        name,
        crate::infra::config_file::setting_path::NOTIFY_EMAIL_PASSWORD
    )
}

/// Parse a boolean setting literal with the synonyms the per-setting
/// validators accept: `true/1/yes/on/enabled` and
/// `false/0/no/off/disabled`. Returns `None` for anything else so the
/// caller can raise a setting-specific error without echoing secrets.
pub fn parse_bool_literal(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "enabled" => Some(true),
        "false" | "0" | "no" | "off" | "disabled" => Some(false),
        _ => None,
    }
}

/// Outcome of `config set`. The value itself is never echoed;
/// only the canonical name and whether the row was updated.
#[derive(Debug, Serialize)]
pub struct ConfigSetOutcome {
    pub setting: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub updated: bool,
}

/// Outcome of `config clear`. `cleared` is true when a row or
/// field existed and was removed.
#[derive(Debug, Serialize)]
pub struct ConfigClearOutcome {
    pub setting: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub cleared: bool,
}
