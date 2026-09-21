//! Tests for the worktree leasing core (issue #239 Phase 1).
//!
//! Coverage spans the four pieces the Phase 1 scope calls out:
//!
//! * **Helpers** (`validate_ref_format`, `compute_fingerprint`,
//!   `slug_from_branch`, `generate_branch`, `parse_worktree_list`,
//!   `repo_identity`, `cache_root_in`) — pure-string / pure-path
//!   unit tests with a `FakeWorktreeRunner` for the git-driven
//!   pieces.
//! * **Storage** (`ensure_schema`, idempotent create, lease round
//!   trip) — exercises the `CREATE TABLE IF NOT EXISTS` migration
//!   through a real `Storage::open_at` against a temp
//!   `PHASEGENT_DB_PATH` so the operator's real database is never
//!   touched.
//! * **Acquisition flows** — idempotent reuse, `no_conflict`
//!   reuse-current-checkout, `new_worktree` for a second session, and
//!   cross-issue isolation. Each test runs in its own temp git repo
//!   under `/tmp` and never mutates the real repository's worktrees.
//! * **Release / dirty probe** — `release_lease` flips the row to
//!   `retained`/`released`, and `is_clean` returns the documented
//!   `Ok(true)` / `Ok(false)` / structured-error values against real
//!   git.

use crate::infra::storage::Storage;
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::worktree::leases::{
    NewLease, heartbeat_active_lease, insert_lease, recover_stale_active_leases,
    stale_active_leases,
};
use crate::worktree::{
    AcquireOutcome, GitOutput, LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED,
    ProcessWorktreeRunner, WorktreeError, WorktreeListEntry, WorktreeRunner, acquire_lease,
    cache_root_in, compute_fingerprint, generate_branch, heartbeat_lease, is_clean,
    leases_for_issue, leases_for_repo, now_unix_secs, parse_worktree_list, release_lease,
    release_lease_forced, repo_identity, slug_from_branch, validate_ref_format, worktree_add,
    worktree_remove,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Test helpers: fake runner, temp repo, temp DB override.
// ---------------------------------------------------------------------------

mod acquire_fallbacks;
mod acquire_isolation;
mod acquire_reuse;
mod git_worktree;
mod heartbeat;
mod helpers;
mod lease_api;
mod release;
mod stale_recovery;
mod storage_schema;
mod support;
