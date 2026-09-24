//! Focused tests for Phase 2 of issue #239 (worktree-seamless).
//!
//! Coverage mirrors the three scope rows the Phase 2 task calls out:
//!
//! * **CLI parsing** — every `worktree` subcommand parses, default
//!   values land where the help advertises them, and the strict
//!   missing-value detection (`--lease` without a value,
//!   non-numeric `--issue`, non-JSON `--format`) is preserved.
//! * **Policy gates** — the role gate is command-level, not
//!   capability-level, and matches the documented split:
//!   `acquire` / `release` / `prune` are orchestrator-only;
//!   `status` / `list` are available to orchestrator, executor, and
//!   reviewer; tester is denied. The executor function returns the
//!   documented `permission` JSON envelope with exit code 3.
//! * **Prune dry-run** — the prune pass classifies every lease
//!   row correctly: active and released rows are skipped, recent
//!   retained rows are skipped, dirty retained rows are
//!   `prunable=false`, and clean + stale + retained rows are
//!   reported as `prunable=true` in dry-run mode. A separate test
//!   drives the same classification against a real temp repo so the
//!   `git status --porcelain` path is exercised end-to-end.
//!
//! All tests run with `PHASEGENT_DB_PATH` pointed at a temp SQLite
//! so the operator's real database is never touched. Each prune
//! integration test also uses its own temp git repo so production
//! worktrees are never mutated.

use crate::cli::branch::execute_branch_context;
use crate::cli::worktree::{
    AcquireJson, PruneAction, PruneCombinedSummary, PruneDisposition, PruneMode, PruneSummary,
    ReleaseStaleAction, ReleaseStaleSummary, execute_worktree, prune_pass,
    scan_worktree_candidates,
};
use crate::command::{Command, IssueCommand, WorktreeCommand};
use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::policy::Role;
use crate::worktree::leases::{NewLease, insert_lease};
use crate::worktree::{
    AcquireOutcome, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED, LeaseRow,
    ProcessWorktreeRunner, WorktreeRunner, acquire_lease, auto_acquire_after_bind, ensure_schema,
    list_for_repo, now_unix_secs, repo_identity, resolve_worktree_auto, worktree_add,
};
use std::path::PathBuf;

mod acquire_cli;
mod acquire_isolation;
mod acquire_surface;
mod bind_cli;
mod cli_support;
mod heartbeat_cli;
mod help_routing;
mod parse_acquire;
mod parse_probe;
mod parse_prune;
mod parse_release;
mod parse_status_list;
mod probe_cli;
mod prune_cli;
mod prune_dry_run;
mod role_gate;
mod support;
