//! Executor and execution model for `worktree prune`.
//!
//! The command has two independent, explicitly opted-in actions. Each
//! has its own read-only candidate scan and its own serialized result
//! envelope, split across adjacent modules so the boundary is visible in
//! the file layout:
//!
//! * [`leases`] — stale active lease recovery (`--release-stale`). The
//!   scan is [`crate::worktree::leases::stale_active_leases`]; the apply
//!   is one `BEGIN IMMEDIATE` transaction that re-checks the predicate.
//! * [`worktrees`] — directory removal (`--remove`). The scan
//!   ([`worktrees::scan_worktree_candidates`]) classifies without
//!   writing; the apply invokes `git worktree remove` only on a clean
//!   worktree.
//!
//! Default `worktree prune` runs both scans in report mode and writes
//! nothing. Branches are never deleted; the only `git` mutation is
//! `git worktree remove` (no `--force`).

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::worktree::leases::{
    ensure_schema, list_for_repo, recover_stale_active_leases, stale_active_leases, stale_window,
};
use crate::worktree::{ProcessWorktreeRunner, now_unix_secs};

use super::{config_error, open_storage, resolve_list_identity, storage_error};

mod leases;
mod worktrees;

pub(crate) use leases::{ReleaseStaleAction, ReleaseStaleSummary};
pub(crate) use worktrees::{
    PruneAction, PruneDisposition, PruneMode, PruneSummary, prune_pass, scan_worktree_candidates,
};

use leases::{stale_recovery_summary, stale_scan_summary};

/// Envelope returned by `worktree prune`. Lease and directory actions are
/// recorded separately so a caller can tell a lease recovery (status
/// flip) apart from a worktree removal; the default report mode writes
/// nothing (`leases.apply` false, `worktrees.dry_run` true).
#[derive(Debug, Serialize)]
pub(crate) struct PruneCombinedSummary {
    pub repo_identity: String,
    pub stale_days: u32,
    pub release_stale: bool,
    pub remove: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub leases: ReleaseStaleSummary,
    pub worktrees: PruneSummary,
}

pub(super) fn execute_prune(
    repo: Option<&str>,
    stale_days: u32,
    release_stale: bool,
    remove: bool,
    reason: Option<String>,
) -> i32 {
    let mut storage = match open_storage() {
        Ok(storage) => storage,
        Err(message) => return config_error(&message),
    };
    if let Err(message) = ensure_schema(&storage) {
        return config_error(&message);
    }
    let identity = match resolve_list_identity(&storage, repo) {
        Ok(identity) => identity,
        Err(message) => return structured_error(message, 2),
    };
    let now = now_unix_secs();
    // One window feeds both the read-only lease scan and the
    // transactional recovery, so the two can never disagree about what
    // "stale" means for this invocation.
    let window = stale_window(now, stale_days);
    // The lease candidate scan is always read-only, even when the caller
    // will apply the recovery below.
    let candidates = match stale_active_leases(&storage, &identity, window.before) {
        Ok(rows) => rows,
        Err(error) => return storage_error(&error),
    };
    let applied_reason = if release_stale {
        match reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(reason) => Some(reason.to_owned()),
            None => {
                return structured_error(
                    serde_json::json!({
                        "kind": "argument",
                        "operation": "worktree prune",
                        "message": "worktree prune --release-stale requires a non-empty --reason",
                    }),
                    2,
                );
            }
        }
    } else {
        None
    };
    // Stale recovery runs before the directory pass so a lease already
    // retained before the invocation is eligible for removal in the same
    // pass. A lease recovered here is refreshed to "now", so the
    // directory scan correctly treats it as recent and keeps its
    // worktree.
    let lease_summary = match applied_reason.as_deref() {
        Some(reason) => {
            match recover_stale_active_leases(&mut storage, &identity, window.before, reason, now) {
                Ok(flipped) => stale_recovery_summary(
                    &identity,
                    stale_days,
                    window.before,
                    now,
                    candidates,
                    &flipped,
                    reason,
                ),
                Err(error) => {
                    return structured_error(
                        serde_json::json!({
                            "kind": error.kind,
                            "operation": "worktree prune",
                            "message": error.message,
                        }),
                        1,
                    );
                }
            }
        }
        None => stale_scan_summary(&identity, stale_days, window.before, now, candidates),
    };
    let repo_path = match repo {
        Some(raw) => std::path::PathBuf::from(raw),
        None => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    };
    let runner = ProcessWorktreeRunner::new();
    let rows = match list_for_repo(&storage, &identity) {
        Ok(rows) => rows,
        Err(error) => return storage_error(&error),
    };
    // `--remove` is the only switch that may delete a directory; without
    // it the pass reports the same classification as a report-only pass.
    let mode = if remove {
        PruneMode::Remove
    } else {
        PruneMode::Report
    };
    let worktree_summary = prune_pass(&storage, &runner, &repo_path, &rows, stale_days, mode);
    let summary = PruneCombinedSummary {
        repo_identity: identity,
        stale_days,
        release_stale,
        remove,
        reason: applied_reason,
        leases: lease_summary,
        worktrees: worktree_summary,
    };
    print_json(&summary)
}
