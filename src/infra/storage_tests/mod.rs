//! Focused tests for the SQLite storage layer.
//!
//! These tests are scoped to the storage module: schema initialisation,
//! role/provider credential separation, and the explicit non-persistence
//! of `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`. End-to-end behaviour that
//! goes through the public auth API lives in the existing
//! `redmine_contract_tests` suite and is intentionally untouched here.
//!
//! Tests use [`Storage::open_at`] with an explicit temp path so they
//! never touch the operator's real platform-standard database.

use crate::auth::{GitlabStoredConfig, RedmineStoredConfig, StoredConfig};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{
    DB_FILENAME, PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE, Storage, TimerRunOwner,
    TimerStatusFilter,
};
use crate::policy::Role;
use std::fs;
use std::path::PathBuf;

mod credentials;
mod projection_claim;
mod projection_liveness;
mod role_config;
mod schema;
mod support;
mod timer_ledger;
mod timer_owner;
