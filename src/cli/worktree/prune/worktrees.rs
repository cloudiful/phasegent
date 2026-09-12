//! Directory half of `worktree prune`: the read-only candidate scan and
//! the removal apply.
//!
//! [`scan_worktree_candidates`] classifies every lease row into a
//! [`PruneDisposition`] without touching storage. Only
//! [`PruneMode::Remove`] may then delete a directory or converge a
//! missing-directory lease row to `released`; [`PruneMode::Report`]
//! serializes the same classification and writes nothing. Branches are
//! never deleted and the only `git` mutation is `git worktree remove`
//! (no `--force`) on a worktree whose porcelain status is empty.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::report_local_warnings;
use crate::infra::storage::Storage;
use crate::worktree::git::{is_clean, worktree_remove};
use crate::worktree::leases::{load_lease, stale_window, update_status};
use crate::worktree::{
    LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED, LeaseRow, WorktreeError,
    WorktreeRunner, now_unix_secs,
};

/// Whether a prune pass only reports candidates or also executes the
/// actions it classified. The parser maps `--remove` onto
/// [`PruneMode::Remove`]; every other invocation is [`PruneMode::Report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PruneMode {
    /// Read-only: classify and report, never write.
    Report,
    /// Execute the removable / missing-directory actions in addition to
    /// reporting them.
    Remove,
}

impl PruneMode {
    fn is_report(self) -> bool {
        matches!(self, PruneMode::Report)
    }
}

/// Read-only classification of one lease row before any write.
///
/// `SkippedDirty` carries the operator-facing reason (real uncommitted
/// changes or a failed probe) so the scan performs no storage work and
/// the apply step owns every mutation. `MissingDirectory` is the one
/// prunable disposition that deletes nothing: it converges the lease row
/// to `released` on apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PruneDisposition {
    SkippedActive,
    SkippedReleased,
    SkippedUnknownStatus,
    SkippedRecent { age_secs: i64, threshold_secs: i64 },
    SkippedDirty(String),
    MissingDirectory,
    Removable,
}

/// One scanned worktree candidate: the lease fields the envelope
/// reports plus the read-only [`PruneDisposition`] the apply step acts
/// on.
#[derive(Debug, Clone)]
pub(crate) struct WorktreeCandidate {
    pub lease_id: String,
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub heartbeat_at: i64,
    pub disposition: PruneDisposition,
}

/// One action of the prune pass. `prunable: false` is the explicit
/// safety signal; the caller can tell at a glance which leases
/// were skipped because they were dirty, still active, or too
/// recent.
#[derive(Debug, Serialize)]
pub(crate) struct PruneAction {
    pub lease_id: String,
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub heartbeat_at: i64,
    pub prunable: bool,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Envelope returned by the directory pass. Stable shape: `dry_run` and
/// `stale_days` echo the inputs so the same call can be replayed
/// from the recorded output.
#[derive(Debug, Serialize)]
pub(crate) struct PruneSummary {
    pub scanned: usize,
    pub candidates: usize,
    pub pruned: usize,
    pub skipped_dirty: usize,
    pub skipped_active_or_released: usize,
    pub skipped_recent: usize,
    pub dry_run: bool,
    pub stale_days: u32,
    pub actions: Vec<PruneAction>,
}

/// Scan every lease row into a read-only candidate set. Storage is not
/// touched and no `git` mutation runs: the only external call is the
/// `is_clean` probe, and its result is captured as a disposition.
pub(crate) fn scan_worktree_candidates(
    runner: &dyn WorktreeRunner,
    rows: &[LeaseRow],
    now: i64,
    threshold_secs: i64,
) -> Vec<WorktreeCandidate> {
    rows.iter()
        .map(|row| scan_candidate(runner, row, now, threshold_secs))
        .collect()
}

fn scan_candidate(
    runner: &dyn WorktreeRunner,
    row: &LeaseRow,
    now: i64,
    threshold_secs: i64,
) -> WorktreeCandidate {
    let base = |disposition: PruneDisposition| WorktreeCandidate {
        lease_id: row.lease_id.clone(),
        worktree_path: row.worktree_path.clone(),
        branch: row.branch.clone(),
        status: row.status.clone(),
        heartbeat_at: row.heartbeat_at,
        disposition,
    };
    match row.status.as_str() {
        LEASE_STATUS_ACTIVE => return base(PruneDisposition::SkippedActive),
        LEASE_STATUS_RELEASED => return base(PruneDisposition::SkippedReleased),
        LEASE_STATUS_RETAINED => {}
        _ => return base(PruneDisposition::SkippedUnknownStatus),
    }
    let age_secs = now.saturating_sub(row.heartbeat_at);
    if age_secs < threshold_secs {
        return base(PruneDisposition::SkippedRecent {
            age_secs,
            threshold_secs,
        });
    }
    let worktree_path = PathBuf::from(&row.worktree_path);
    if !worktree_path.exists() {
        return base(PruneDisposition::MissingDirectory);
    }
    match is_clean(runner, &worktree_path) {
        Ok(true) => base(PruneDisposition::Removable),
        Ok(false) => base(PruneDisposition::SkippedDirty(
            "worktree has uncommitted changes".to_owned(),
        )),
        Err(error) => base(PruneDisposition::SkippedDirty(format!(
            "is_clean probe failed: {}",
            error.message
        ))),
    }
}

/// The prune pass. Public within the crate so the focused tests
/// can drive it without a full CLI round-trip. Candidate scanning is
/// read-only; the returned actions are produced by applying each
/// candidate under `mode`, and only [`PruneMode::Remove`] may delete a
/// directory or flip a lease. Branches are never deleted; only
/// `git worktree remove` is invoked, and only on a clean worktree. The
/// lease row is flipped to `released` only on a successful removal (or
/// when the directory was already gone) so the table keeps an audit
/// trail of what was actually removed.
pub(crate) fn prune_pass(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    rows: &[LeaseRow],
    stale_days: u32,
    mode: PruneMode,
) -> PruneSummary {
    let now = now_unix_secs();
    let window = stale_window(now, stale_days);
    let candidates = scan_worktree_candidates(runner, rows, now, window.threshold_secs);
    let actions: Vec<PruneAction> = candidates
        .iter()
        .map(|candidate| apply_worktree_candidate(storage, runner, repo_path, candidate, mode))
        .collect();
    summarize_prune_actions(actions, rows.len(), mode, stale_days)
}

fn apply_worktree_candidate(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    candidate: &WorktreeCandidate,
    mode: PruneMode,
) -> PruneAction {
    let mut action = PruneAction {
        lease_id: candidate.lease_id.clone(),
        worktree_path: candidate.worktree_path.clone(),
        branch: candidate.branch.clone(),
        status: candidate.status.clone(),
        heartbeat_at: candidate.heartbeat_at,
        prunable: false,
        result: "noop".to_owned(),
        message: None,
    };
    match &candidate.disposition {
        PruneDisposition::SkippedActive => {
            action.result = "skipped_active".to_owned();
            action.message = Some("active leases are never pruned".to_owned());
        }
        PruneDisposition::SkippedReleased => {
            action.result = "skipped_released".to_owned();
            action.message = Some("already released; no worktree to remove".to_owned());
        }
        PruneDisposition::SkippedUnknownStatus => {
            action.result = "skipped_released".to_owned();
            action.message = Some(format!(
                "unrecognised lease status {}; treated as released",
                candidate.status
            ));
        }
        PruneDisposition::SkippedRecent {
            age_secs,
            threshold_secs,
        } => {
            action.result = "skipped_recent".to_owned();
            action.message = Some(format!(
                "heartbeat age {age_secs}s is below stale threshold {threshold_secs}s"
            ));
        }
        PruneDisposition::SkippedDirty(reason) => {
            action.result = "skipped_dirty".to_owned();
            action.message = Some(reason.clone());
        }
        PruneDisposition::MissingDirectory => {
            // The directory is already gone but the lease row still
            // records it. Mark the row as released so the operator can
            // see the audit trail converge.
            action.prunable = true;
            action.message = Some("worktree directory already absent".to_owned());
            if mode == PruneMode::Remove {
                if let Err(error) =
                    update_status(storage, &candidate.lease_id, LEASE_STATUS_RELEASED)
                {
                    action.message = Some(format!("could not mark released: {error}"));
                } else if let Err(error) =
                    reload_lease_branch(storage, &candidate.lease_id, &mut action)
                {
                    let _ = error;
                }
            }
        }
        PruneDisposition::Removable => {
            if mode.is_report() {
                action.prunable = true;
                action.result = "prunable".to_owned();
                action.message = Some("dry-run: would call git worktree remove".to_owned());
            } else {
                apply_removal(storage, runner, repo_path, candidate, &mut action);
            }
        }
    }
    action
}

fn apply_removal(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    candidate: &WorktreeCandidate,
    action: &mut PruneAction,
) {
    let worktree_path = PathBuf::from(&candidate.worktree_path);
    match worktree_remove(runner, repo_path, &worktree_path) {
        Ok(()) => match update_status(storage, &candidate.lease_id, LEASE_STATUS_RELEASED) {
            Ok(()) => {
                action.prunable = true;
                action.result = "pruned".to_owned();
                action.message = Some("git worktree remove succeeded".to_owned());
            }
            Err(error) => {
                action.prunable = true;
                action.result = "skipped_dirty".to_owned();
                action.message = Some(format!(
                    "worktree removed but lease flip failed: {}",
                    error.message
                ));
            }
        },
        Err(error) => {
            report_local_warnings(
                "worktree prune",
                Some(format!(
                    "git worktree remove failed for {}: {}",
                    worktree_path.display(),
                    error.message
                )),
            );
            action.result = "skipped_dirty".to_owned();
            action.message = Some(format!("worktree_remove error: {}", error.message));
        }
    }
}

fn summarize_prune_actions(
    actions: Vec<PruneAction>,
    scanned: usize,
    mode: PruneMode,
    stale_days: u32,
) -> PruneSummary {
    let dry_run = mode.is_report();
    let mut summary = PruneSummary {
        scanned,
        candidates: 0,
        pruned: 0,
        skipped_dirty: 0,
        skipped_active_or_released: 0,
        skipped_recent: 0,
        dry_run,
        stale_days,
        actions: Vec::with_capacity(actions.len()),
    };
    for action in actions {
        if action.prunable && !dry_run && action.result == "pruned" {
            summary.pruned += 1;
        }
        if action.result == "prunable" {
            summary.candidates += 1;
        }
        match action.result.as_str() {
            "skipped_dirty" => summary.skipped_dirty += 1,
            "skipped_active" | "skipped_released" => summary.skipped_active_or_released += 1,
            "skipped_recent" => summary.skipped_recent += 1,
            _ => {}
        }
        summary.actions.push(action);
    }
    summary
}

fn reload_lease_branch(
    storage: &Storage,
    lease_id: &str,
    action: &mut PruneAction,
) -> Result<(), WorktreeError> {
    if let Some(updated) = load_lease(storage, lease_id)? {
        action.status = updated.status;
    }
    Ok(())
}
