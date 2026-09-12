//! Lease half of `worktree prune`: the read-only candidate scan report
//! and the applied stale-recovery report.
//!
//! Both are built from the same `Vec<LeaseRow>` snapshot captured before
//! the invocation, so the report-only and apply envelopes stay
//! comparable. The recovery's flipped set is intersected with that
//! snapshot: a heartbeat that raced the scan cannot leave an unflipped
//! row labelled `retained`.

use serde::Serialize;

use crate::worktree::{LEASE_STATUS_RETAINED, LeaseRow};

/// One candidate row in the lease-recovery report. `status` is the
/// resulting status: `active` for a report-only candidate, `retained`
/// after an applied recovery.
#[derive(Debug, Serialize)]
pub(crate) struct ReleaseStaleAction {
    pub lease_id: String,
    pub issue: u64,
    pub session: String,
    pub worktree_path: String,
    pub branch: String,
    pub heartbeat_at: i64,
    pub age_secs: i64,
    pub status: String,
}

/// Lease half of the combined `worktree prune` envelope. `apply` echoes
/// `--release-stale`; `candidates` is the read-only pre-apply count and
/// `released` is the number of rows the transaction actually flipped.
#[derive(Debug, Serialize)]
pub(crate) struct ReleaseStaleSummary {
    pub repo_identity: String,
    pub stale_days: u32,
    pub stale_before: i64,
    pub apply: bool,
    pub candidates: usize,
    pub released: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub leases: Vec<ReleaseStaleAction>,
}

pub(super) fn stale_scan_summary(
    identity: &str,
    stale_days: u32,
    stale_before: i64,
    now: i64,
    candidates: Vec<LeaseRow>,
) -> ReleaseStaleSummary {
    let candidate_count = candidates.len();
    ReleaseStaleSummary {
        repo_identity: identity.to_owned(),
        stale_days,
        stale_before,
        apply: false,
        candidates: candidate_count,
        released: 0,
        reason: None,
        leases: candidates
            .into_iter()
            .map(|row| {
                let status = row.status.clone();
                lease_action(now, row, status)
            })
            .collect(),
    }
}

pub(super) fn stale_recovery_summary(
    identity: &str,
    stale_days: u32,
    stale_before: i64,
    now: i64,
    candidates: Vec<LeaseRow>,
    flipped: &[LeaseRow],
    reason: &str,
) -> ReleaseStaleSummary {
    let flipped_ids: std::collections::HashSet<&str> =
        flipped.iter().map(|row| row.lease_id.as_str()).collect();
    let candidate_count = candidates.len();
    ReleaseStaleSummary {
        repo_identity: identity.to_owned(),
        stale_days,
        stale_before,
        apply: true,
        candidates: candidate_count,
        released: flipped.len(),
        reason: Some(reason.to_owned()),
        leases: candidates
            .into_iter()
            .filter(|row| flipped_ids.contains(row.lease_id.as_str()))
            .map(|row| lease_action(now, row, LEASE_STATUS_RETAINED.to_owned()))
            .collect(),
    }
}

fn lease_action(now: i64, row: LeaseRow, status: String) -> ReleaseStaleAction {
    ReleaseStaleAction {
        age_secs: now.saturating_sub(row.heartbeat_at),
        status,
        lease_id: row.lease_id,
        issue: row.issue,
        session: row.session,
        worktree_path: row.worktree_path,
        branch: row.branch,
        heartbeat_at: row.heartbeat_at,
    }
}
