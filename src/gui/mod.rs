//! Single-binary desktop shell behind the `gui` Cargo feature.
//!
//! Normal CLI commands never initialize the GUI: only the explicit
//! `phasegent gui` entry (and the conservative no-argument desktop
//! heuristic in `main`) calls [`run`]. The GUI reuses the existing
//! crate modules directly — [`crate::infra::storage::Storage`],
//! [`crate::config_snapshot`], [`crate::config`],
//! [`crate::auth`], [`crate::providers::config`],
//! [`crate::providers::ProviderDispatcher`],
//! [`crate::branch_context`] — instead of duplicating storage or
//! provider logic.
//!
//! Typed IPC surface (all redacted, no credential values):
//!
//! - `get_app_metadata` reports binary name/version/identifier.
//! - `get_config_snapshot` returns the redacted
//!   [`crate::config_snapshot::ConfigSnapshot`] used by `config show`.
//! - `get_branch_context` reports the current branch-bound issue id.
//! - `get_tasks` resolves role/provider via existing dispatch and
//!   fetches a bounded provider page (`search_issue_page`); the
//!   index `block_on` bridge is never called from the Tauri runtime.
//! - `get_status` reports branch, bound issue, sanitised endpoint,
//!   local timers, and Redmine status support (`not_supported` clean).
//! - `set_config_setting` / `clear_config_setting` mutate non-secret
//!   settings through the existing config write facade.
//! - `set_credential` / `clear_credential` use the dedicated secure
//!   storage path (write-only secrets, presence/length responses).
//! - `get_provisioning_status` reports admin-provisioned Redmine
//!   identity via existing `auth` APIs without exposing API keys.
//!
//! Blocking provider/storage work never runs on the Tauri async
//! runtime directly; async commands bridge through
//! `tauri::async_runtime::spawn_blocking`.
//!
//! Layout: [`models`] holds the redacted IPC shapes, [`validate`]
//! the pure input/redaction helpers, [`backend`] the blocking
//! task/status reads, [`settings`] the config/credential mutations,
//! and [`commands`] the feature-gated Tauri command set.

mod backend;
mod commands;
mod models;
mod settings;
mod validate;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use backend::{read_branch_context, read_status_blocking, read_tasks_blocking};
pub use commands::run;
#[allow(unused_imports)]
pub use models::{
    BranchContextPayload, ClearCredentialRequest, ClearCredentialResponse, ClearSettingRequest,
    ClearSettingResponse, CredentialPresence, ProvisioningQuery, ProvisioningStatus,
    SetCredentialRequest, SetSettingRequest, SetSettingResponse, StatusPayload, StatusRequest,
    TaskEntry, TasksPayload, TasksRequest, TimerDto,
};
#[allow(unused_imports)]
pub use settings::{
    clear_credential_blocking, clear_setting_blocking, read_provisioning_blocking,
    write_credential_blocking, write_setting_blocking,
};
#[allow(unused_imports)]
pub use validate::{
    bound_message, canonical_non_secret_setting, validate_credential_value, validate_setting_value,
    validate_task_limit, validate_task_state,
};
#[allow(unused_imports)]
pub(crate) use validate::{
    bound_title, now_fetched_at, parse_provider_optional, parse_provider_required,
    parse_role_optional, parse_role_required, parse_role_with_default, sanitize_optional_url,
};

use serde::Serialize;

/// Redacted application identity exposed to the frontend shell.
/// Contains no secrets, paths, or credentials.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct AppMetadata {
    /// Binary/product name (`phasegent`).
    pub name: &'static str,
    /// Crate version (`CARGO_PKG_VERSION`).
    pub version: &'static str,
    /// Tauri application identifier (matches `tauri.conf.json`).
    pub identifier: &'static str,
}

/// Shared metadata constructor used by both the CLI stub and the
/// Tauri command so the value stays in sync with the manifest.
#[allow(dead_code)]
pub fn app_metadata() -> AppMetadata {
    AppMetadata {
        name: "phasegent",
        version: env!("CARGO_PKG_VERSION"),
        identifier: "com.cloud1ful.phasegent",
    }
}

/// Read the redacted configuration snapshot through the existing
/// `config show` facade. Secrets are never echoed: credentials report
/// presence/length only and the repository URL is sanitised by
/// [`crate::config_snapshot`]. Notify channel secrets stay write-only
/// the same way; notify URLs render sanitised.
#[allow(dead_code)]
pub fn read_config_snapshot() -> Result<crate::config_snapshot::ConfigSnapshot, String> {
    let storage = crate::infra::storage::Storage::open().map_err(validate::bound_message)?;
    crate::config_snapshot::render(&storage, None).map_err(validate::bound_message)
}

/// Content hash of the embedded `frontend/dist` tree written by
/// `build.rs` to `OUT_DIR` during a gui build. Because this is included
/// via `include_str!` it is a real compile input: any dist change alters
/// the crate's inputs and busts the Cargo fingerprint, so `cargo install`
/// re-embeds even when no Rust source changed. Returns a fallback when
/// the binary was built without gui (no `OUT_DIR` hash) so the field stays
/// stable and warning-free.
#[allow(dead_code)]
pub fn frontend_dist_hash() -> String {
    #[cfg(feature = "gui")]
    {
        include_str!(concat!(env!("OUT_DIR"), "/frontend_dist.hash")).to_owned()
    }
    #[cfg(not(feature = "gui"))]
    {
        "unavailable".to_owned()
    }
}
