//! `phasegent config show` and `phasegent --role <ROLE> config import-env`.
//!
//! The two commands cooperate so operators can inspect a redacted
//! snapshot of the local SQLite database and explicitly persist
//! current process environment values. Ordinary provider commands
//! never touch this module: persistence is opt-in via `import-env`
//! so a stray `PHASEGENT_*` shell variable never lands in the
//! database behind an operator's back.
//!
//! `show` is safe to invoke without `--role` so an operator can read
//! the global picture (database path, every role's provider/URL,
//! credential presence/length, and the global setting summaries).
//! When `--role` is supplied the snapshot is the same JSON with the
//! `roles` object restricted to that single role; nothing about the
//! command's behaviour changes so the calling test surface stays
//! minimal.
//!
//! `import-env` requires `--role` because most persisted settings are
//! role-scoped. The global settings (`PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`,
//! `PHASEGENT_REDMINE_REPOSITORY_URL`, and the machine-wide
//! `PHASEGENT_DEFAULT_PROVIDER`) are still written in the same call
//! so a single invocation captures everything the operator wants to
//! ship.
//!
//! The machine-wide default provider is also managed by the
//! `config provider get | set | clear` subcommands documented
//! elsewhere; this module exposes the underlying helpers
//! (`provider_get`, `provider_set`, `provider_clear`) so the CLI
//! layer stays free of SQLite details.
//!
//! Snapshot rendering and credential redaction live in the sibling
//! [`config_snapshot`] module so this facade stays focused on
//! dispatching and persistence.

use crate::auth::{GitlabStoredConfig, RedmineStoredConfig, StoredConfig};
use crate::config_snapshot;
use crate::policy::Role;
use crate::provider_config::ProviderKind;
use crate::storage::Storage;
use serde::Serialize;
use serde_json::Value;
use std::str::FromStr;

/// Names of the global settings the storage layer understands. Kept
/// in sync with `storage_schema::GLOBAL_SETTING_NAMES`; `config
/// import-env` and `config show` both iterate over this list when
/// they need to touch every canonical entry.
pub(crate) const GLOBAL_SETTING_NAMES: &[&str] = &[
    "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
    "PHASEGENT_REDMINE_REPOSITORY_URL",
    "PHASEGENT_DEFAULT_PROVIDER",
];

/// Environment variables persisted by `config import-env` for the
/// given role, in display order. Names map 1:1 to environment
/// variable names so the JSON output mirrors what the operator typed
/// in their shell.
const ROLE_SCOPED_ENV_NAMES: &[&str] = &[
    "PHASEGENT_PROVIDER",
    "PHASEGENT_API_BASE",
    "PHASEGENT_REPOSITORY",
    "PHASEGENT_REDMINE_API_BASE",
    "PHASEGENT_REDMINE_PROJECT_ID",
    "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
    "PHASEGENT_GITLAB_API_BASE",
    "PHASEGENT_GITLAB_PROJECT_ID",
    "PHASEGENT_PROJECT_ID",
    "PHASEGENT_CLOSE_STATUS_ID",
];

/// Names that are role-scoped and must be treated as secrets. Listed
/// separately so the JSON output can flag the secret names without
/// ever returning their values.
const SECRET_ROLE_SCOPED_ENV_NAMES: &[&str] = &[
    // No role-scoped secret is currently imported: Forgejo tokens and
    // Redmine API keys go through `auth setup` only. The list is kept
    // empty by design so future contributors add a name here only when
    // `import-env` actually persists it.
];

/// Names of secret-flavored global settings. Used by `config show` to
/// label the bearer key summary without printing its value. The
/// machine-wide default provider is intentionally NOT in this list
/// because the literal values (`forgejo`, `redmine`, `gitlab`) are
/// non-secret and the resolver reads them verbatim.
const SECRET_GLOBAL_SETTING_NAMES: &[&str] = &["PHASEGENT_REDMINE_GIT_MIRROR_API_KEY"];

/// Build a redacted snapshot of the local SQLite database. `role`
/// restricts the `roles` array when supplied; passing `None` returns
/// every known role.
pub fn show(role: Option<Role>) -> Result<Value, String> {
    let storage = Storage::open()?;
    crate::auth::ensure_legacy_import(&storage)?;
    let snapshot = config_snapshot::render(&storage, role)?;
    serde_json::to_value(snapshot)
        .map_err(|error| format!("could not encode config snapshot: {error}"))
}

/// Outcome of a single `config import-env` invocation. The JSON
/// payload echoes which role-scoped fields and global settings were
/// imported; secret names are flagged with `secret: true` and their
/// values are never returned.
#[derive(Debug, Serialize)]
pub struct ImportOutcome {
    role: &'static str,
    pub(crate) role_scoped: Vec<ImportEntry>,
    pub(crate) global_settings: Vec<ImportEntry>,
    pub(crate) imported: usize,
    pub(crate) skipped: usize,
}

#[derive(Debug, Serialize, Clone, Copy)]
pub struct ImportEntry {
    pub name: &'static str,
    pub imported: bool,
    pub secret: bool,
}

impl ImportEntry {
    fn imported(name: &'static str) -> Self {
        Self {
            name,
            imported: true,
            secret: SECRET_ROLE_SCOPED_ENV_NAMES.contains(&name)
                || SECRET_GLOBAL_SETTING_NAMES.contains(&name),
        }
    }

    fn skipped(name: &'static str) -> Self {
        Self {
            name,
            imported: false,
            secret: SECRET_ROLE_SCOPED_ENV_NAMES.contains(&name)
                || SECRET_GLOBAL_SETTING_NAMES.contains(&name),
        }
    }
}

/// Stable list of role-scoped environment variables understood by
/// `config import-env`. Exposed so the CLI help text can list the
/// canonical names without duplicating the schema-side constants.
#[allow(dead_code)]
pub fn role_scoped_env_names() -> &'static [&'static str] {
    ROLE_SCOPED_ENV_NAMES
}

/// Persist every currently-set `PHASEGENT_*` environment variable
/// the import flow understands for `role`. Returns the structured
/// outcome so the CLI layer can render a JSON report without echoing
/// any secret value.
pub fn import_env(role: Role) -> Result<ImportOutcome, String> {
    let storage = Storage::open()?;
    crate::auth::ensure_legacy_import(&storage)?;
    let mut role_scoped = Vec::with_capacity(ROLE_SCOPED_ENV_NAMES.len());
    let mut imported = 0_usize;
    let mut skipped = 0_usize;
    for &name in ROLE_SCOPED_ENV_NAMES {
        match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => {
                apply_role_env(&storage, role, name, value.trim())?;
                role_scoped.push(ImportEntry::imported(name));
                imported += 1;
            }
            _ => {
                role_scoped.push(ImportEntry::skipped(name));
                skipped += 1;
            }
        }
    }

    let mut global_entries = Vec::with_capacity(GLOBAL_SETTING_NAMES.len());
    for &name in GLOBAL_SETTING_NAMES {
        // The machine-wide default provider goes through the same
        // ProviderKind validator the resolver uses so a typo in the
        // shell never lands in SQLite. Other global settings (mirror
        // bearer key, mirror repository URL) are stored verbatim.
        match read_env_trimmed(name) {
            Some(value) => {
                if name == "PHASEGENT_DEFAULT_PROVIDER" {
                    let kind: ProviderKind = value.parse().map_err(|error| {
                        format!("could not parse PHASEGENT_DEFAULT_PROVIDER '{value}': {error}")
                    })?;
                    storage.save_global_setting(name, kind.as_str())?;
                } else {
                    storage.save_global_setting(name, &value)?;
                }
                global_entries.push(ImportEntry::imported(name));
                imported += 1;
            }
            None => {
                global_entries.push(ImportEntry::skipped(name));
                skipped += 1;
            }
        }
    }

    Ok(ImportOutcome {
        role: role.as_str(),
        role_scoped,
        global_settings: global_entries,
        imported,
        skipped,
    })
}

/// Apply a single role-scoped environment variable to `storage`.
/// Provider selection goes through the schema-level `update_provider`
/// helper so the existing precedence between `auth setup` and the
/// raw `provider` column stays consistent. Redmine-specific keys
/// update `role_redmine_config`; the generic `PHASEGENT_API_BASE`
/// writes to both `role_config.api_base` (Forgejo) and
/// `role_redmine_config.api_base` (Redmine) so the resolver's
/// env-over-SQLite fallback path works for either provider when only
/// the generic variable is set. Empty / whitespace-only values are
/// silently skipped so `import-env` never erases a previously-stored
/// field with a stray `export PHASEGENT_*=""`.
fn apply_role_env(storage: &Storage, role: Role, name: &str, value: &str) -> Result<(), String> {
    match name {
        "PHASEGENT_PROVIDER" => {
            let provider = ProviderKind::from_str(value).map_err(|error| {
                format!("could not parse PHASEGENT_PROVIDER '{value}': {error}")
            })?;
            storage.update_provider(role, provider.as_str())?;
        }
        "PHASEGENT_API_BASE" => {
            // Generic API base: persist under both columns so a
            // Redmine `RedmineConfig::resolve()` whose env vars are
            // empty can still pick it up via the role_redmine_config
            // fallback. The provider-specific `PHASEGENT_REDMINE_API_BASE`
            // is processed later in `import_env` and overwrites this
            // value when both are present.
            update_role_config_field(storage, role, |config| {
                config.api_base = Some(value.to_owned());
            })?;
            update_redmine_config_field(storage, role, |config| {
                config.api_base = Some(value.to_owned());
            })?;
            // Phase-1 GitLab foundation: the generic alias also lands
            // on the Gitlab row so `GitlabConfig::resolve` keeps
            // working after a restart even when only the generic
            // `PHASEGENT_API_BASE` is exported. The provider-specific
            // `PHASEGENT_GITLAB_API_BASE` is processed later and
            // overrides this value when both are present.
            update_gitlab_config_field(storage, role, |config| {
                config.api_base = Some(value.to_owned());
            })?;
        }
        "PHASEGENT_REPOSITORY" => {
            update_role_config_field(storage, role, |config| {
                config.repository = Some(value.to_owned());
            })?;
        }
        "PHASEGENT_REDMINE_API_BASE" => {
            update_redmine_config_field(storage, role, |config| {
                config.api_base = Some(value.to_owned());
            })?;
        }
        "PHASEGENT_REDMINE_PROJECT_ID" => {
            update_redmine_config_field(storage, role, |config| {
                config.project_id = Some(value.to_owned());
            })?;
        }
        "PHASEGENT_REDMINE_CLOSE_STATUS_ID" => {
            let parsed = value.parse::<u64>().map_err(|error| {
                format!("could not parse PHASEGENT_REDMINE_CLOSE_STATUS_ID '{value}': {error}")
            })?;
            update_redmine_config_field(storage, role, |config| {
                config.close_status_id = Some(parsed);
            })?;
        }
        "PHASEGENT_GITLAB_API_BASE" => {
            update_gitlab_config_field(storage, role, |config| {
                config.api_base = Some(value.to_owned());
            })?;
        }
        "PHASEGENT_GITLAB_PROJECT_ID" => {
            // GitLab project ids are numeric so the value is parsed
            // up front and the parse error stays actionable; an empty
            // value falls through to a no-op so a stray
            // `export PHASEGENT_GITLAB_PROJECT_ID=""` never erases the
            // previously-stored id.
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(());
            }
            let parsed = trimmed.parse::<u64>().map_err(|error| {
                format!("could not parse PHASEGENT_GITLAB_PROJECT_ID '{value}': {error}")
            })?;
            if parsed == 0 {
                return Err("PHASEGENT_GITLAB_PROJECT_ID must be greater than zero".to_owned());
            }
            update_gitlab_config_field(storage, role, |config| {
                config.project_id = Some(parsed);
            })?;
        }
        "PHASEGENT_PROJECT_ID" => {
            // Generic alias that the resolver falls back to. Persist
            // it under the Redmine row so the alias continues to work
            // after a restart even when PHASEGENT_REDMINE_PROJECT_ID
            // is unset. Phase-1 GitLab foundation: the same generic
            // alias is also parsed numerically and persisted under
            // the Gitlab row. Per-string values that fail the
            // numeric parse still land on the Redmine row so the
            // legacy behaviour does not regress.
            update_redmine_config_field(storage, role, |config| {
                config.project_id = Some(value.to_owned());
            })?;
            let trimmed = value.trim();
            if !trimmed.is_empty()
                && let Ok(parsed) = trimmed.parse::<u64>()
                && parsed > 0
            {
                update_gitlab_config_field(storage, role, |config| {
                    config.project_id = Some(parsed);
                })?;
            }
        }
        "PHASEGENT_CLOSE_STATUS_ID" => {
            let parsed = value.parse::<u64>().map_err(|error| {
                format!("could not parse PHASEGENT_CLOSE_STATUS_ID '{value}': {error}")
            })?;
            update_redmine_config_field(storage, role, |config| {
                config.close_status_id = Some(parsed);
            })?;
        }
        other => {
            return Err(format!(
                "config import-env does not recognise environment variable '{other}'"
            ));
        }
    }
    Ok(())
}

fn update_role_config_field(
    storage: &Storage,
    role: Role,
    mutate: impl FnOnce(&mut StoredConfig),
) -> Result<(), String> {
    let mut config = storage.load_role_config(role)?.unwrap_or_default();
    mutate(&mut config);
    storage.save_role_config(role, &config)
}

fn update_redmine_config_field(
    storage: &Storage,
    role: Role,
    mutate: impl FnOnce(&mut RedmineStoredConfig),
) -> Result<(), String> {
    let mut config = storage.load_redmine_config(role)?.unwrap_or_default();
    mutate(&mut config);
    storage.save_redmine_config(role, &config)
}

fn update_gitlab_config_field(
    storage: &Storage,
    role: Role,
    mutate: impl FnOnce(&mut GitlabStoredConfig),
) -> Result<(), String> {
    let mut config = storage.load_gitlab_config(role)?.unwrap_or_default();
    mutate(&mut config);
    storage.save_gitlab_config(role, &config)
}

fn read_env_trimmed(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

/// Helper used by the CLI layer to render `ImportOutcome` as JSON.
pub fn import_env_json(role: Role) -> Result<Value, String> {
    let outcome = import_env(role)?;
    serde_json::to_value(outcome)
        .map_err(|error| format!("could not encode import-env outcome: {error}"))
}

/// Helper used by the CLI layer to render `ConfigSnapshot` as JSON.
pub fn show_json(role: Option<Role>) -> Result<Value, String> {
    show(role)
}

/// Outcome of `config provider get`. `provider` is `null` when the
/// machine-wide default has never been set so operators can
/// distinguish "unset" from "explicitly forgejo".
#[derive(Debug, Serialize)]
pub struct ProviderGetOutcome {
    pub provider: Option<&'static str>,
}

/// Read the persisted `PHASEGENT_DEFAULT_PROVIDER` row. The value
/// is validated through `ProviderKind::from_str` so a stale row
/// that contains an unknown literal surfaces a structured config
/// error rather than silently overriding the resolver.
pub fn provider_get() -> Result<ProviderGetOutcome, String> {
    let storage = Storage::open()?;
    crate::auth::ensure_legacy_import(&storage)?;
    match storage.load_global_setting("PHASEGENT_DEFAULT_PROVIDER")? {
        Some(value) => {
            let kind: ProviderKind = value.parse().map_err(|error| {
                format!("persisted PHASEGENT_DEFAULT_PROVIDER is invalid: {error}")
            })?;
            Ok(ProviderGetOutcome {
                provider: Some(kind.as_str()),
            })
        }
        None => Ok(ProviderGetOutcome { provider: None }),
    }
}

/// Validate and persist `PHASEGENT_DEFAULT_PROVIDER`. The value
/// must round-trip through `ProviderKind::from_str` so a typo never
/// lands in the database; the resolver consumes the same parser
/// later, so the validation rules are identical end to end.
pub fn provider_set(value: &str) -> Result<ProviderGetOutcome, String> {
    let kind: ProviderKind = value
        .parse()
        .map_err(|error| format!("invalid provider '{value}': {error}"))?;
    let storage = Storage::open()?;
    crate::auth::ensure_legacy_import(&storage)?;
    storage.save_global_setting("PHASEGENT_DEFAULT_PROVIDER", kind.as_str())?;
    Ok(ProviderGetOutcome {
        provider: Some(kind.as_str()),
    })
}

/// Remove the persisted `PHASEGENT_DEFAULT_PROVIDER` row. Returns
/// `cleared: true` when the row existed and was removed; returns
/// `cleared: false` when the default was already absent so the
/// operator can tell the two apart without inspecting the
/// database.
#[derive(Debug, Serialize)]
pub struct ProviderClearOutcome {
    pub cleared: bool,
}

pub fn provider_clear() -> Result<ProviderClearOutcome, String> {
    let storage = Storage::open()?;
    crate::auth::ensure_legacy_import(&storage)?;
    let cleared = storage.delete_global_setting("PHASEGENT_DEFAULT_PROVIDER")?;
    Ok(ProviderClearOutcome { cleared })
}
