//! Desktop backends served to the Electron shell through the hidden
//! stdio bridge.
//!
//! Normal CLI commands never initialize a desktop shell: the Rust binary is
//! the CLI executable and the packaged
//! [`crate::desktop_bridge`] companion for the Electron application, and the
//! window lifecycle lives in that application. The backends here are the
//! single source of truth for desktop payloads, redaction, and error text:
//! [`crate::desktop_bridge`] dispatches the same blocking entry points on its
//! own worker threads, so the packaged shell gets identical payloads,
//! redaction, and error text. The GUI reuses the existing
//! crate modules directly — [`crate::infra::storage::Storage`],
//! [`crate::config_snapshot`], [`crate::config`],
//! [`crate::auth`], [`crate::providers::config`],
//! [`crate::providers::ProviderDispatcher`],
//! [`crate::branch_links`] — instead of duplicating storage or
//! provider logic.
//!
//! Typed IPC surface (all redacted, no credential values):
//!
//! - `get_app_metadata` reports binary name/version/identifier.
//! - `get_config_snapshot` returns the redacted
//!   [`crate::config_snapshot::ConfigSnapshot`] used by `config show`.
//! - `get_branch_context` reports the current branch's durable-linked (or
//!   branch-name) issue id.
//! - `get_tasks` resolves role/provider via existing dispatch and
//!   fetches a bounded provider page (`search_issue_page`); the
//!   index `block_on` bridge is never called from a bridge worker.
//! - `get_status` reports branch, bound issue, sanitised endpoint,
//!   local timers, and Redmine status support (`not_supported` clean).
//! - `set_config_setting` / `clear_config_setting` mutate non-secret
//!   settings through the existing config write facade.
//! - `set_credential` / `clear_credential` use the dedicated secure
//!   storage path (write-only secrets, presence/length responses).
//! - `get_provisioning_status` reports admin-provisioned Redmine
//!   identity via existing `auth` APIs without exposing API keys.
//!
//! Blocking provider/storage work never blocks the bridge's request handling:
//! [`crate::desktop_bridge`] dispatches the same blocking entry points on its
//! own worker threads.
//!
//! Layout: [`models`] holds the redacted IPC shapes, [`validate`]
//! the pure input/redaction helpers, [`backend`] the blocking
//! task/status reads, and [`settings`] the config/credential mutations.

mod backend;
mod models;
mod settings;
mod validate;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use backend::{read_branch_context, read_status_blocking, read_tasks_blocking};
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
    pub name: &'static str,
    pub version: &'static str,
    /// Electron application identifier (matches `electron-builder.yml`).
    pub identifier: &'static str,
}

/// Shared metadata constructor so the reported identity stays in sync with
/// the packaged application manifest.
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

/// Content hash of the renderer bundle the desktop shell reports on its
/// Status page.
///
/// The Electron application serves the renderer from its own package, so this
/// binary does not embed `frontend/dist`; a packaging step that knows the
/// shipped bundle may provide its hash through the
/// `PHASEGENT_FRONTEND_DIST_HASH` build-time variable. Without one the field
/// keeps a stable placeholder so it stays serializable and warning-free.
#[allow(dead_code)]
pub fn frontend_dist_hash() -> String {
    option_env!("PHASEGENT_FRONTEND_DIST_HASH")
        .filter(|value| !value.is_empty())
        .unwrap_or("unavailable")
        .to_owned()
}
