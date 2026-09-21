//! Focused tests for the `config show`, `config set`/`clear`, and
//! `config provider` flows.
//!
//! These tests cover:
//! * `config show` redacts credentials
//! * `config set`/`clear` parser acceptance/rejection, alias handling,
//!   secret handling via `--stdin` / interactive path, and persistence
//! * `config import-env` removal
//! * `project list` does not require a project id
//! * provider get/set/clear and snapshot reporting
//!
//! Tests build a fresh `Storage` via [`Storage::open_at`] against a
//! private temp database.

use crate::auth;
use crate::command::{self, Command, ProjectCommand};
use crate::config;
use crate::config_write;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{
    DB_FILENAME, PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE, Storage,
};
use crate::policy::Role;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

mod config_snapshot;
mod index_backend;
mod index_backend_validation;
mod parse_show;
mod project_list;
mod provider_default;
mod resolution_fallback;
mod set_clear_parse;
mod set_clear_persist;
mod show_redaction;
mod support;
mod toml_precedence;
mod toml_validation;
