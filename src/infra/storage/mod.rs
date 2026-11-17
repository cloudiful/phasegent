//! SQLite-backed persistence for phasegent business state.
//!
//! The storage module keeps the execution ledger, the agent notification
//! outbox, and the canonical task identity tables in a single SQLite
//! database. The database lives at the OS-standard config location
//! returned by [`directories::ProjectDirs`]: `~/.config/phasegent`
//! on Linux, `~/Library/Application Support/com.Cloud1ful.phasegent` on
//! macOS, and `%APPDATA%\Cloud1ful\phasegent\config` on Windows.
//!
//! Configuration and credentials are not stored here. They live in the
//! JSONC configuration file next to this database, resolved through
//! [`crate::infra::config_file`]. This module therefore creates no
//! configuration table and reads no configuration value.
//!
//! The schema lives in [`crate::infra::storage_schema`] so the data model
//! stays readable as a whole; this file stays focused as a thin aggregator
//! that re-exports the public surface and declares focused child modules.

mod connection;
mod notifications;

pub use connection::Storage;
pub use notifications::NotificationRecord;

pub(crate) use crate::infra::storage_schema::{
    PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE,
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
