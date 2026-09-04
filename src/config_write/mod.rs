//! Explicit `config set` / `config clear` persistence helpers.
//!
//! The module owns the canonical setting map, validation, and
//! storage persistence for the explicit CLI configuration surface
//! that replaced `config import-env`. Environment variables remain
//! runtime-only overrides in `crate::auth` and `crate::providers::config`;
//! this module only touches SQLite.
//!
//! Secret handling: `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` is never
//! accepted as a positional or option value. The CLI dispatch rejects
//! direct values before this module persists anything; the module
//! itself never echoes secret values in errors or JSON.
//!
//! Global settings are machine-wide and usable without `--role`;
//! role-scoped settings require a role and operate on the per-role
//! rows.

mod clear;
mod common;
mod set;

pub use clear::clear_setting;
pub use set::dispatch_set;
#[cfg(test)]
pub use set::{set_setting_stdin_content, set_setting_value};

use serde::Serialize;

/// All canonical setting names the explicit surface understands.
/// The strings double as the canonical environment variable names
/// so `config show` and the resolver can share the same literals.
/// Project-id aliases were removed in Phase 1 (remove-project-id);
/// `PHASEGENT_REDMINE_PROJECT_ID`, `PHASEGENT_GITLAB_PROJECT_ID`, and
/// the ambiguous `PHASEGENT_PROJECT_ID` are intentionally absent and
/// rejected as unknown settings. `PHASEGENT_INDEX_BACKEND` remains only
/// for compatibility: backend selection is URL-driven
/// (`PHASEGENT_INDEX_PG_URL` presence selects PostgreSQL) and the legacy
/// backend value is ignored for selection.
pub(crate) const ALL_CANONICAL: &[&str] = &[
    "PHASEGENT_PROVIDER",
    "PHASEGENT_API_BASE",
    "PHASEGENT_REPOSITORY",
    "PHASEGENT_REDMINE_API_BASE",
    "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
    "PHASEGENT_GITLAB_API_BASE",
    "PHASEGENT_CLOSE_STATUS_ID",
    "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
    "PHASEGENT_REDMINE_REPOSITORY_URL",
    "PHASEGENT_DEFAULT_PROVIDER",
    "PHASEGENT_INDEX_BACKEND",
    "PHASEGENT_INDEX_PG_URL",
];

const GLOBAL_SETTINGS: &[&str] = &[
    "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
    "PHASEGENT_REDMINE_REPOSITORY_URL",
    "PHASEGENT_DEFAULT_PROVIDER",
    "PHASEGENT_INDEX_BACKEND",
    "PHASEGENT_INDEX_PG_URL",
];

const SECRET_SETTINGS: &[&str] = &[
    "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
    "PHASEGENT_INDEX_PG_URL",
];

/// Resolve a user-supplied setting name to its canonical form.
///
/// Accepts both the canonical `PHASEGENT_*` names and concise
/// kebab-case aliases (e.g. `redmine-git-mirror-api-key`,
/// `api-base`, `default-provider`). Matching is case-insensitive
/// and hyphen/underscore agnostic, with an optional `PHASEGENT_`
/// prefix for the short forms.
pub fn canonical_setting_name(input: &str) -> Option<&'static str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    for &canonical in ALL_CANONICAL {
        if trimmed == canonical {
            return Some(canonical);
        }
    }
    let upper = trimmed.to_ascii_uppercase().replace('-', "_");
    for &canonical in ALL_CANONICAL {
        if upper == canonical {
            return Some(canonical);
        }
    }
    let with_prefix = format!("PHASEGENT_{upper}");
    for &canonical in ALL_CANONICAL {
        if with_prefix == canonical {
            return Some(canonical);
        }
    }
    None
}

pub fn is_global_setting(name: &str) -> bool {
    GLOBAL_SETTINGS.contains(&name)
}

pub fn is_secret_setting(name: &str) -> bool {
    SECRET_SETTINGS.contains(&name)
}

pub fn is_role_scoped_setting(name: &str) -> bool {
    !is_global_setting(name)
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
