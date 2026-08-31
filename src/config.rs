//! `phasegent config show`, `config set`, `config clear`, and `config provider`.
//!
//! The commands cooperate so operators can inspect a redacted snapshot
//! of the local SQLite database and explicitly persist settings.
//! `show` is safe to invoke without `--role` so an operator can read
//! the global picture. `config set` / `config clear` persist a single
//! setting at a time; global settings are machine-wide and usable
//! without `--role`, while role-scoped settings require `--role`.
//! The mirror bearer key is never accepted as a direct value; it
//! must be supplied via `--stdin` or the secure interactive prompt.
//!
//! Snapshot rendering and credential redaction live in the sibling
//! [`config_snapshot`] module; set/clear persistence lives in
//! [`crate::config_write`] so this facade stays focused.

use crate::config_snapshot;
use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::config::ProviderKind;
use serde::Serialize;
use serde_json::Value;

/// Build a redacted snapshot of the local SQLite database. `role`
/// restricts the `roles` array when supplied; passing `None` returns
/// every known role.
pub fn show(role: Option<Role>, storage: &Storage) -> Result<Value, String> {
    let snapshot = config_snapshot::render(storage, role)?;
    serde_json::to_value(snapshot)
        .map_err(|error| format!("could not encode config snapshot: {error}"))
}

/// Helper used by the CLI layer to render `ConfigSnapshot` as JSON.
pub fn show_json(role: Option<Role>, storage: &Storage) -> Result<Value, String> {
    show(role, storage)
}

/// Dispatch `config set` with the already-resolved canonical setting.
/// `value` is the optional positional value; `use_stdin` is the
/// `--stdin` flag. Secret settings reject direct values.
pub fn set_json(
    role: Option<Role>,
    canonical: &str,
    value: Option<&str>,
    use_stdin: bool,
    storage: &Storage,
) -> Result<Value, String> {
    crate::config_write::dispatch_set(role, canonical, value, use_stdin, storage)
}

/// Dispatch `config clear` for the canonical setting.
pub fn clear_json(role: Option<Role>, canonical: &str, storage: &Storage) -> Result<Value, String> {
    crate::config_write::clear_setting(role, canonical, storage)
}

/// Outcome of `config provider get`. `provider` is `null` when the
/// machine-wide default has never been set.
#[derive(Debug, Serialize)]
pub struct ProviderGetOutcome {
    pub provider: Option<&'static str>,
}

/// Read the persisted `PHASEGENT_DEFAULT_PROVIDER` row.
pub fn provider_get(storage: &Storage) -> Result<ProviderGetOutcome, String> {
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

/// Validate and persist `PHASEGENT_DEFAULT_PROVIDER`.
pub fn provider_set(value: &str, storage: &Storage) -> Result<ProviderGetOutcome, String> {
    let kind: ProviderKind = value
        .parse()
        .map_err(|error| format!("invalid provider '{value}': {error}"))?;
    storage.save_global_setting("PHASEGENT_DEFAULT_PROVIDER", kind.as_str())?;
    Ok(ProviderGetOutcome {
        provider: Some(kind.as_str()),
    })
}

/// Remove the persisted `PHASEGENT_DEFAULT_PROVIDER` row.
#[derive(Debug, Serialize)]
pub struct ProviderClearOutcome {
    pub cleared: bool,
}

pub fn provider_clear(storage: &Storage) -> Result<ProviderClearOutcome, String> {
    let cleared = storage.delete_global_setting("PHASEGENT_DEFAULT_PROVIDER")?;
    Ok(ProviderClearOutcome { cleared })
}
