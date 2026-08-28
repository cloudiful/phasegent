//! SQLite-backed persistence for phasegent role configuration and credentials.
//!
//! The storage module keeps role/provider configuration, per-role
//! credentials, and the small set of machine-wide settings in a single
//! SQLite database. The database lives at the OS-standard config
//! location returned by [`directories::ProjectDirs`]: `~/.config/phasegent`
//! on Linux, `~/Library/Application Support/com.Cloud1ful.phasegent` on
//! macOS, and `%APPDATA%\Cloud1ful\phasegent\config` on Windows. The
//! schema splits role-scoped provider configuration from role/provider
//! credentials so `config show` can mask secrets without leaking their
//! content.
//!
//! The schema lives in [`crate::infra::storage_schema`] so the data model
//! stays readable as a whole; this file stays focused as a thin aggregator
//! that re-exports the public surface and declares focused child modules.

mod config;
mod connection;
mod credentials;
mod global_settings;

#[cfg(test)]
pub(crate) mod test_support;

pub use connection::Storage;
pub use global_settings::GlobalSettingSummary;

#[allow(unused_imports)]
pub(crate) use crate::infra::storage_schema::{
    DB_FILENAME, PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE,
};

/// Re-export the canonical global setting names so callers do not
/// need to depend on `storage_schema` directly.
pub(crate) use crate::infra::storage_schema::{
    GLOBAL_REDMINE_GIT_MIRROR_API_KEY, GLOBAL_REDMINE_REPOSITORY_URL,
};

#[allow(unused_imports)]
pub(crate) use crate::infra::timer_ledger::{
    PROJECTION_LEASE_SECS, PROJECTION_TOKEN_BOUND, TIMER_STATUS_RUNNING, TIMER_SYNC_FAILED,
    TIMER_SYNC_PENDING, TIMER_SYNC_PROJECTING, TIMER_SYNC_SYNCED, TIMER_SYNC_UNCONFIRMED,
    valid_timer_sync_status,
};
pub use crate::infra::timer_ledger::{TimerRun, TimerRunOwner, TimerStatusFilter};
