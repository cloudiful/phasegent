//! Focused tests for the worktree directory CLI surface.
//!
//! Coverage mirrors the directory contract: parsing of the remaining
//! subcommands, the role gates, the read-only probe/status/list surfaces, and
//! the acquire reuse/isolation decisions. Every store-backed test points
//! `PHASEGENT_DB_PATH` at a temporary SQLite file, so the operator's real
//! database is never touched, and every Git-backed test uses its own temporary
//! repository.

pub(crate) use crate::cli::worktree::execute_worktree;
use crate::command::{Command, WorktreeCommand};
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::worktree::{ProcessWorktreeRunner, WorktreeRunner, repo_identity};
use std::path::PathBuf;

mod acquire_surface;
mod help_routing;
mod parse_acquire;
mod parse_probe;
mod parse_prune;
mod parse_status_list;
mod probe_cli;
mod registry_read;
mod role_gate;
mod support;
