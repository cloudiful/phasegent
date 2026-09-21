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

pub fn show(role: Option<Role>, storage: &Storage) -> Result<Value, String> {
    let snapshot = config_snapshot::render(storage, role)?;
    serde_json::to_value(snapshot)
        .map_err(|error| format!("could not encode config snapshot: {error}"))
}

pub fn show_json(role: Option<Role>, storage: &Storage) -> Result<Value, String> {
    show(role, storage)
}

pub fn set_json(
    role: Option<Role>,
    canonical: &str,
    value: Option<&str>,
    use_stdin: bool,
    storage: &Storage,
) -> Result<Value, String> {
    crate::config_write::dispatch_set(role, canonical, value, use_stdin, storage)
}

pub fn clear_json(role: Option<Role>, canonical: &str, storage: &Storage) -> Result<Value, String> {
    crate::config_write::clear_setting(role, canonical, storage)
}

/// Outcome of `config provider get`. `provider` is `null` when the
/// machine-wide default has never been set.
#[derive(Debug, Serialize)]
pub struct ProviderGetOutcome {
    pub provider: Option<&'static str>,
}

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

pub fn provider_set(value: &str, storage: &Storage) -> Result<ProviderGetOutcome, String> {
    let kind: ProviderKind = value
        .parse()
        .map_err(|error| format!("invalid provider '{value}': {error}"))?;
    storage.save_global_setting("PHASEGENT_DEFAULT_PROVIDER", kind.as_str())?;
    Ok(ProviderGetOutcome {
        provider: Some(kind.as_str()),
    })
}

#[derive(Debug, Serialize)]
pub struct ProviderClearOutcome {
    pub cleared: bool,
}

pub fn provider_clear(storage: &Storage) -> Result<ProviderClearOutcome, String> {
    let cleared = storage.delete_global_setting("PHASEGENT_DEFAULT_PROVIDER")?;
    Ok(ProviderClearOutcome { cleared })
}
