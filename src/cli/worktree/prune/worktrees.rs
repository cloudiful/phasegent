//! Directory half of `worktree prune`: the read-only candidate scan and
//! the removal apply.
//!
//! Since issue 916 the unit of candidacy is a **canonical directory**, not a
//! lease row: every lease row whose `worktree_path` resolves to the same
//! directory is grouped together and considered once. A directory is removed
//! only when
//!
//! 1. no `active` lease references it — including a row belonging to the same
//!    session, and including the invocation-start snapshot so a stale recovery
//!    in the same call can never make an active directory removable;
//! 2. every associated issue is *confirmed closed* by
//!    [`IssueStateSource`][super::issue_state::IssueStateSource];
//! 3. the shared candidate guards clear it (never the main checkout, never
//!    dirty or untracked, never a directory the OpenCode host still — or
//!    cannot be proven not to — occupy, in `Sync` semantics that never move a
//!    session);
//! 4. `git worktree remove` (no `--force`) succeeds after a fresh re-read of
//!    rows and guards.
//!
//! Age never gates directory deletion: a freshly retained, closed, clean
//! directory is removable immediately. `--stale-days` remains only for the
//! explicit stale-active recovery half. Lease rows and branches are preserved
//! as history.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::report_local_warnings;
use crate::infra::storage::Storage;
use crate::lifecycle::cleanup::CleanupMode;
use crate::lifecycle::cleanup::TargetResolver;
use crate::lifecycle::cleanup::candidate::{local_protected_reason, protected_reason};
use crate::worktree::git::worktree_remove;
use crate::worktree::leases::{list_all_for_repo, load_lease, update_status};
use crate::worktree::opencode::OpenCodeApi;
use crate::worktree::{
    LEASE_STATUS_ACTIVE, LEASE_STATUS_RELEASED, LEASE_STATUS_RETAINED, LeaseRow, WorktreeRunner,
    canonical_or_self,
};

use super::child_retirement;
use super::host_cache::ProjectedHost;
use super::issue_state::{IssueClosure, IssueStateSource};

/// Whether a prune pass only reports candidates or also executes the
/// actions it classified. The executor maps a non-preview invocation onto
/// [`PruneMode::Remove`]; `--dryrun` selects [`PruneMode::Report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PruneMode {
    /// Read-only: classify and report, never write.
    Report,
    /// Execute the removals in addition to reporting them.
    Remove,
}

impl PruneMode {
    fn is_report(self) -> bool {
        matches!(self, PruneMode::Report)
    }
}

/// One repository scope of a prune pass: its canonical identity, the
/// checkout the shared guards validate against, and the rows recorded
/// against it.
pub(crate) struct PruneScope<'a> {
    pub identity: &'a str,
    pub repo_path: &'a Path,
    pub rows: &'a [LeaseRow],
}

/// Read-only classification of one canonical directory before any write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PruneDisposition {
    /// At least one `active` lease referenced the directory.
    SkippedActive,
    /// A lease row carried an unrecognised status; kept safely.
    SkippedUnknownStatus,
    /// The directory does not exist; counted separately, nothing removed.
    MissingDirectory,
    /// A shared candidate guard kept the directory.
    SkippedProtected(String),
    /// An associated issue is not confirmed closed.
    SkippedIssue(String),
    /// Every condition held; the directory may be removed.
    Removable,
}

/// Every row that resolves to one canonical directory.
#[derive(Debug, Clone)]
pub(crate) struct DirectoryCandidate {
    pub canonical_path: PathBuf,
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub heartbeat_at: i64,
    pub lease_ids: Vec<String>,
    pub issues: Vec<u64>,
    pub disposition: PruneDisposition,
    /// Finished child sessions the read-only scan proved eligible to retire
    /// (issue 918 P2). Empty unless the candidate is removable-by-retirement.
    pub retire: Vec<String>,
}

/// One directory action of the prune pass. `prunable` is the explicit safety
/// signal; the caller can tell at a glance which directories were removed,
/// which are absent, and which were kept and why.
#[derive(Debug, Serialize)]
pub(crate) struct PruneAction {
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub heartbeat_at: i64,
    pub lease_ids: Vec<String>,
    pub issues: Vec<u64>,
    pub prunable: bool,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Finished child sessions confirmed relocated out of the directory before
    /// removal. This is factual: a child moved and confirmed here stays listed
    /// even when a later occupancy/closure/removal check keeps the directory.
    /// Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relocated_children: Vec<String>,
    /// Finished child sessions a *preview* would relocate. Proposals only:
    /// never populated by a removal pass, and the two lists are therefore
    /// distinct. Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub planned_children: Vec<String>,
}

/// Envelope returned by the directory pass for one repository. All counts are
/// directory-based, so a lease row is never mistaken for a directory.
#[derive(Debug, Serialize)]
pub(crate) struct PruneSummary {
    pub scanned: usize,
    pub removed: usize,
    pub eligible: usize,
    pub absent: usize,
    pub kept_active: usize,
    pub kept_unknown: usize,
    pub kept_protected: usize,
    pub kept_issue: usize,
    pub failed: usize,
    pub dry_run: bool,
    pub stale_days: u32,
    pub actions: Vec<PruneAction>,
}

impl PruneSummary {
    /// Directories kept for any safety reason.
    pub(crate) fn kept(&self) -> usize {
        self.kept_active + self.kept_unknown + self.kept_protected + self.kept_issue
    }
}

/// One grouped directory before classification.
struct DirectoryGroup {
    canonical_path: PathBuf,
    worktree_path: String,
    branch: String,
    status: String,
    heartbeat_at: i64,
    lease_ids: Vec<String>,
    issues: Vec<u64>,
    rows: Vec<LeaseRow>,
}

/// Group lease rows by canonical directory path, in a deterministic order.
fn group_rows(rows: &[LeaseRow]) -> Vec<DirectoryGroup> {
    let mut groups: Vec<DirectoryGroup> = Vec::new();
    let mut index: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
    // `rows` arrive newest-first; sorting the output keeps the pass stable
    // regardless of row order.
    let mut ordered: Vec<&LeaseRow> = rows.iter().collect();
    ordered.sort_by(|a, b| a.lease_id.cmp(&b.lease_id));
    for row in ordered {
        let canonical = canonical_or_self(Path::new(&row.worktree_path));
        match index.get(&canonical) {
            Some(position) => {
                let group = &mut groups[*position];
                group.lease_ids.push(row.lease_id.clone());
                if !group.issues.contains(&row.issue) {
                    group.issues.push(row.issue);
                }
                group.rows.push(row.clone());
                if group.status != row.status {
                    group.status = "mixed".to_owned();
                }
            }
            None => {
                index.insert(canonical.clone(), groups.len());
                groups.push(DirectoryGroup {
                    canonical_path: canonical,
                    worktree_path: row.worktree_path.clone(),
                    branch: row.branch.clone(),
                    status: row.status.clone(),
                    heartbeat_at: row.heartbeat_at,
                    lease_ids: vec![row.lease_id.clone()],
                    issues: vec![row.issue],
                    rows: vec![row.clone()],
                });
            }
        }
    }
    for group in &mut groups {
        group.issues.sort_unstable();
    }
    groups.sort_by(|a, b| a.canonical_path.cmp(&b.canonical_path));
    groups
}

/// Scan every lease row into a read-only directory candidate set. Storage is
/// not touched and no `git` mutation runs. The issue-state source is consulted
/// only for a directory that already passed the active, absent, unknown, and
/// shared-guard checks, so no active/missing/main-checkout path resolves a
/// credential.
pub(crate) fn scan_worktree_candidates(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    mode: PruneMode,
) -> Vec<DirectoryCandidate> {
    let target = TargetResolver::new(scope.repo_path, scope.identity);
    group_rows(scope.rows)
        .into_iter()
        .map(|group| {
            classify(
                runner,
                api,
                scope,
                &target,
                pinned_active,
                source,
                mode,
                group,
            )
        })
        .collect()
}

/// Classify one directory group. Read-only.
#[allow(clippy::too_many_arguments)]
fn classify(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    target: &TargetResolver<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    mode: PruneMode,
    group: DirectoryGroup,
) -> DirectoryCandidate {
    let classification = classify_group(
        runner,
        api,
        scope,
        target,
        pinned_active,
        source,
        mode,
        &group,
    );
    DirectoryCandidate {
        canonical_path: group.canonical_path,
        worktree_path: group.worktree_path,
        branch: group.branch,
        status: group.status,
        heartbeat_at: group.heartbeat_at,
        lease_ids: group.lease_ids,
        issues: group.issues,
        disposition: classification.disposition,
        retire: classification.retire,
    }
}

/// A directory's disposition plus the finished children a removal would first
/// retire (issue 918 P2).
struct Classification {
    disposition: PruneDisposition,
    retire: Vec<String>,
}

impl Classification {
    fn kept(disposition: PruneDisposition) -> Self {
        Self {
            disposition,
            retire: Vec::new(),
        }
    }
}

/// The ordered decision for one directory: active, absent, unknown, guard,
/// closure, removable. When the only thing keeping the directory is a set of
/// individually proven finished child sessions, the shared guard is evaluated
/// through a read-only projection that hides exactly those children, and the
/// removable result carries them for retirement at apply time.
#[allow(clippy::too_many_arguments)]
fn classify_group(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    target: &TargetResolver<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    mode: PruneMode,
    group: &DirectoryGroup,
) -> Classification {
    let canonical = group.canonical_path.to_string_lossy().into_owned();
    if group
        .rows
        .iter()
        .any(|row| row.status == LEASE_STATUS_ACTIVE)
        || pinned_active.contains(&canonical)
    {
        return Classification::kept(PruneDisposition::SkippedActive);
    }
    if group.rows.iter().any(|row| {
        row.status != LEASE_STATUS_ACTIVE
            && row.status != LEASE_STATUS_RETAINED
            && row.status != LEASE_STATUS_RELEASED
    }) {
        // An unrecognised status is kept safely before anything else is
        // inferred from the path.
        return Classification::kept(PruneDisposition::SkippedUnknownStatus);
    }
    if !group.canonical_path.exists() {
        return Classification::kept(PruneDisposition::MissingDirectory);
    }
    // The local Git guards run first and read nothing from the host: a main
    // checkout, an active peer's path, or a dirty/untracked directory is
    // excluded here, so it never reaches `child_retirement.plan`'s occupancy
    // reads and cannot report an irrelevant session reason.
    if let Some(reason) = local_protected_reason(
        runner,
        scope.identity,
        scope.rows,
        None,
        &representative(group),
    ) {
        return Classification::kept(PruneDisposition::SkippedProtected(reason));
    }
    // Read-only retirement planning. A plan that keeps the directory reports
    // its own precise reason; a vacant/retire plan lets the shared guard judge
    // every remaining occupant and local guard.
    let plan = child_retirement::plan(api, target, &group.canonical_path, scope.rows);
    let plan_keep = match &plan {
        child_retirement::RetirementPlan::Keep(reason) => Some(reason.clone()),
        _ => None,
    };
    let children = plan.children().to_vec();
    let projected = ProjectedHost::new(api, &children);
    let guard_api: &dyn OpenCodeApi = if children.is_empty() { api } else { &projected };
    match guard_verdict(runner, guard_api, scope, target, source, mode, group) {
        Some(PruneDisposition::SkippedProtected(reason)) => {
            // The plan's own reason is more specific than the generic
            // "hosted here and not associated" from the shared guard.
            Classification::kept(PruneDisposition::SkippedProtected(
                plan_keep.unwrap_or(reason),
            ))
        }
        Some(other) => Classification::kept(other),
        None => {
            // A plan that kept the directory must never leave it removable.
            if let Some(reason) = plan_keep {
                Classification::kept(PruneDisposition::SkippedProtected(reason))
            } else {
                Classification {
                    disposition: PruneDisposition::Removable,
                    retire: children,
                }
            }
        }
    }
}

/// The shared guard's verdict for one directory (local guards, then occupancy,
/// then closure), or `None` when the directory may be removed. The occupancy
/// half reads through whatever `api` is passed: the raw host, or the read-only
/// projection that hides already-proven finished children.
#[allow(clippy::too_many_arguments)]
fn guard_verdict(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    target: &TargetResolver<'_>,
    source: &dyn IssueStateSource,
    mode: PruneMode,
    group: &DirectoryGroup,
) -> Option<PruneDisposition> {
    if let Some(reason) = protected_reason(
        runner,
        api,
        cleanup_mode_for(mode),
        target,
        scope.identity,
        scope.rows,
        None,
        &representative(group),
    ) {
        return Some(PruneDisposition::SkippedProtected(reason));
    }
    match closure_of(source, scope, group) {
        IssueClosure::Closed => None,
        IssueClosure::Keep(reason) => Some(PruneDisposition::SkippedIssue(reason)),
    }
}

fn cleanup_mode_for(mode: PruneMode) -> CleanupMode {
    // Both modes only ever *look*: `Sync` checks occupancy and never moves a
    // session, so a preview may discover the host read-only while a removal
    // re-proves it. Only the close chain is allowed to move a session; prune's
    // own, narrowly-scoped retire step (issue 918 P2) moves only proven
    // finished children, outside this shared guard.
    match mode {
        PruneMode::Report | PruneMode::Remove => CleanupMode::Sync,
    }
}

fn representative(group: &DirectoryGroup) -> LeaseRow {
    group.rows.first().cloned().unwrap_or_else(|| LeaseRow {
        lease_id: String::new(),
        repo_identity: String::new(),
        issue: 0,
        session: String::new(),
        checkout_path: String::new(),
        worktree_path: group.worktree_path.clone(),
        branch: group.branch.clone(),
        status: group.status.clone(),
        created_at: group.heartbeat_at,
        heartbeat_at: group.heartbeat_at,
        release_reason: None,
    })
}

/// Closure requires *every* associated issue to be confirmed closed.
fn closure_of(
    source: &dyn IssueStateSource,
    scope: &PruneScope<'_>,
    group: &DirectoryGroup,
) -> IssueClosure {
    let mut keep: Vec<String> = Vec::new();
    for issue in &group.issues {
        let verdict = source.closure(scope.repo_path, *issue);
        if !verdict.is_closed()
            && let Some(reason) = verdict.keep_reason()
        {
            keep.push(reason.to_owned());
        }
    }
    if keep.is_empty() {
        IssueClosure::Closed
    } else {
        IssueClosure::Keep(keep.join("; "))
    }
}

/// The prune pass. Candidate scanning is read-only; only [`PruneMode::Remove`]
/// may delete a directory, and only after a fresh re-read of rows and guards
/// through `fresh_api` (never the scan cache).
///
/// `scan_api` may be a memoizing view for the classification pass, but every
/// removal authorization re-reads through `fresh_api`, so a stale scan answer
/// can never authorize a deletion.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prune_pass(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    scan_api: &dyn OpenCodeApi,
    fresh_api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    stale_days: u32,
    mode: PruneMode,
    progress: &dyn Fn(&str),
) -> PruneSummary {
    progress(&format!(
        "scanning {} ({} lease record(s), mode {})",
        scope.repo_path.display(),
        scope.rows.len(),
        if mode.is_report() {
            "preview"
        } else {
            "removal"
        }
    ));
    let candidates = scan_worktree_candidates(runner, scan_api, scope, pinned_active, source, mode);
    if !mode.is_report()
        && candidates
            .iter()
            .any(|c| c.disposition == PruneDisposition::Removable)
    {
        progress(&format!(
            "checking host and issue state for {} ({} directory(ies))",
            scope.repo_path.display(),
            candidates.len()
        ));
    }
    let actions: Vec<PruneAction> = candidates
        .iter()
        .map(|candidate| {
            apply_candidate(
                storage,
                runner,
                fresh_api,
                scope,
                pinned_active,
                source,
                candidate,
                mode,
                progress,
            )
        })
        .collect();
    summarize(actions, candidates.len(), mode, stale_days)
}

#[allow(clippy::too_many_arguments)]
fn apply_candidate(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    candidate: &DirectoryCandidate,
    mode: PruneMode,
    progress: &dyn Fn(&str),
) -> PruneAction {
    let mut action = PruneAction {
        worktree_path: candidate.worktree_path.clone(),
        branch: candidate.branch.clone(),
        status: candidate.status.clone(),
        heartbeat_at: candidate.heartbeat_at,
        lease_ids: candidate.lease_ids.clone(),
        issues: candidate.issues.clone(),
        prunable: false,
        result: "noop".to_owned(),
        message: None,
        relocated_children: Vec::new(),
        planned_children: Vec::new(),
    };
    match &candidate.disposition {
        PruneDisposition::SkippedActive => {
            action.result = "skipped_active".to_owned();
            action.message = Some("active lease references this directory".to_owned());
        }
        PruneDisposition::SkippedUnknownStatus => {
            action.result = "skipped_unknown".to_owned();
            action.message = Some("unrecognised lease status; kept safely".to_owned());
        }
        PruneDisposition::SkippedProtected(reason) => {
            action.result = "skipped_protected".to_owned();
            action.message = Some(reason.clone());
        }
        PruneDisposition::SkippedIssue(reason) => {
            action.result = "skipped_issue".to_owned();
            action.message = Some(reason.clone());
        }
        PruneDisposition::MissingDirectory => {
            action.result = "absent".to_owned();
            action.message = Some("worktree directory already absent".to_owned());
            if mode == PruneMode::Remove {
                // The directory is gone; converge every absent row to
                // `released` so the audit trail matches reality. No branch,
                // directory, or unrelated row is touched.
                for lease_id in &candidate.lease_ids {
                    if let Err(error) = update_status(storage, lease_id, LEASE_STATUS_RELEASED) {
                        action.result = "failed".to_owned();
                        action.message = Some(format!("could not mark released: {error}"));
                        break;
                    }
                }
                if action.result == "absent" {
                    reload_statuses(storage, &candidate.lease_ids, &mut action);
                }
            }
        }
        PruneDisposition::Removable => {
            if mode.is_report() {
                action.prunable = true;
                action.result = "prunable".to_owned();
                action.message = Some("dry-run: would call git worktree remove".to_owned());
                // Report what a removal would relocate first; nothing moves, and
                // the proposal is a distinct field so it is never labelled as an
                // actual relocation.
                action.planned_children = candidate.retire.clone();
            } else {
                progress(&format!(
                    "verifying {} before removal",
                    candidate.worktree_path
                ));
                // Re-read rows and guards immediately before removal so a
                // state change between scan and apply keeps the directory, and
                // retire only freshly re-proven finished children.
                let outcome = recheck(
                    storage,
                    runner,
                    api,
                    scope,
                    pinned_active,
                    source,
                    mode,
                    candidate,
                    progress,
                );
                action.relocated_children = outcome.moved;
                match outcome.keep_reason {
                    Some(reason) => {
                        action.result = "skipped_protected".to_owned();
                        action.message = Some(format!("kept on re-check: {reason}"));
                    }
                    None => apply_removal(storage, runner, scope, candidate, &mut action),
                }
            }
        }
    }
    action
}

/// Re-evaluate one directory against fresh rows and guards, retiring any freshly
/// re-proven finished children before removal. `keep_reason` is `Some` when the
/// directory must survive; `moved` names the children whose relocation to the
/// main checkout was confirmed (even when a later step kept the directory).
struct Recheck {
    keep_reason: Option<String>,
    moved: Vec<String>,
}

impl Recheck {
    fn proceed() -> Self {
        Self {
            keep_reason: None,
            moved: Vec::new(),
        }
    }

    fn kept(reason: String) -> Self {
        Self {
            keep_reason: Some(reason),
            moved: Vec::new(),
        }
    }
}

/// Re-evaluate one directory against fresh rows and guards. Returns the keep
/// reason (if any) and the children whose relocation was confirmed.
#[allow(clippy::too_many_arguments)]
fn recheck(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    pinned_active: &BTreeSet<String>,
    source: &dyn IssueStateSource,
    mode: PruneMode,
    candidate: &DirectoryCandidate,
    progress: &dyn Fn(&str),
) -> Recheck {
    let fresh = match list_all_for_repo(storage, scope.identity) {
        Ok(rows) => rows,
        Err(error) => return Recheck::kept(format!("could not re-read leases: {}", error.message)),
    };
    let fresh_scope = PruneScope {
        identity: scope.identity,
        repo_path: scope.repo_path,
        rows: &fresh,
    };
    let target = TargetResolver::new(scope.repo_path, scope.identity);
    let Some(fresh_group) = group_rows(&fresh)
        .into_iter()
        .find(|group| group.canonical_path == candidate.canonical_path)
    else {
        // The candidate's rows vanished between scan and apply; the scan
        // already proved the directory removable, and no row remains to
        // protect, so removal may proceed.
        return Recheck::proceed();
    };
    let classification = classify_group(
        runner,
        api,
        &fresh_scope,
        &target,
        pinned_active,
        source,
        mode,
        &fresh_group,
    );
    if classification.disposition != PruneDisposition::Removable {
        return Recheck::kept(recheck_reason(classification.disposition));
    }
    if classification.retire.is_empty() {
        return Recheck::proceed();
    }
    // Retire the freshly re-proven children, then re-prove vacancy through the
    // original, uncached host and fresh rows before any removal.
    let owners = child_retirement::lease_owners(&fresh, &candidate.canonical_path);
    progress(&format!(
        "retiring {} finished child session(s) from {}",
        classification.retire.len(),
        candidate.worktree_path
    ));
    let outcome = child_retirement::retire(
        api,
        &target,
        &candidate.canonical_path,
        &owners,
        &classification.retire,
    );
    if let Some(failure) = outcome.failure {
        return Recheck {
            keep_reason: Some(format!("retirement stopped: {failure}")),
            moved: outcome.moved,
        };
    }
    match guard_verdict(
        runner,
        api,
        &fresh_scope,
        &target,
        source,
        mode,
        &fresh_group,
    ) {
        None => Recheck {
            keep_reason: None,
            moved: outcome.moved,
        },
        Some(verdict) => Recheck {
            keep_reason: Some(format!(
                "kept after retirement: {}",
                recheck_reason(verdict)
            )),
            moved: outcome.moved,
        },
    }
}

fn recheck_reason(disposition: PruneDisposition) -> String {
    match disposition {
        PruneDisposition::Removable => String::new(),
        PruneDisposition::SkippedActive => "an active lease re-appeared".to_owned(),
        PruneDisposition::SkippedUnknownStatus => "a lease status became unrecognised".to_owned(),
        PruneDisposition::SkippedProtected(reason) | PruneDisposition::SkippedIssue(reason) => {
            reason
        }
        PruneDisposition::MissingDirectory => "the directory vanished".to_owned(),
    }
}

fn apply_removal(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    scope: &PruneScope<'_>,
    candidate: &DirectoryCandidate,
    action: &mut PruneAction,
) {
    let worktree_path = PathBuf::from(&candidate.worktree_path);
    match worktree_remove(runner, scope.repo_path, &worktree_path) {
        Ok(()) => {
            let mut flip_failed = false;
            for lease_id in &candidate.lease_ids {
                if let Err(error) = update_status(storage, lease_id, LEASE_STATUS_RELEASED) {
                    action.prunable = true;
                    action.result = "failed".to_owned();
                    action.message = Some(format!(
                        "worktree removed but lease flip failed: {}",
                        error.message
                    ));
                    flip_failed = true;
                    break;
                }
            }
            if !flip_failed {
                action.prunable = true;
                action.result = "pruned".to_owned();
                action.message = Some("git worktree remove succeeded".to_owned());
            }
        }
        Err(error) => {
            report_local_warnings(
                "worktree prune",
                Some(format!(
                    "git worktree remove failed for {}: {}",
                    worktree_path.display(),
                    error.message
                )),
            );
            action.result = "failed".to_owned();
            action.message = Some(format!("worktree_remove error: {}", error.message));
        }
    }
}

fn reload_statuses(storage: &Storage, lease_ids: &[String], action: &mut PruneAction) {
    for lease_id in lease_ids {
        if let Ok(Some(updated)) = load_lease(storage, lease_id) {
            action.status = updated.status;
        }
    }
}

fn summarize(
    actions: Vec<PruneAction>,
    scanned: usize,
    mode: PruneMode,
    stale_days: u32,
) -> PruneSummary {
    let mut summary = PruneSummary {
        scanned,
        removed: 0,
        eligible: 0,
        absent: 0,
        kept_active: 0,
        kept_unknown: 0,
        kept_protected: 0,
        kept_issue: 0,
        failed: 0,
        dry_run: mode.is_report(),
        stale_days,
        actions: Vec::with_capacity(actions.len()),
    };
    for action in actions {
        match action.result.as_str() {
            "pruned" => summary.removed += 1,
            "prunable" => summary.eligible += 1,
            "absent" => summary.absent += 1,
            "skipped_active" => summary.kept_active += 1,
            "skipped_unknown" => summary.kept_unknown += 1,
            "skipped_protected" => summary.kept_protected += 1,
            "skipped_issue" => summary.kept_issue += 1,
            "failed" => summary.failed += 1,
            _ => {}
        }
        summary.actions.push(action);
    }
    summary
}

/// The set of canonical directory paths that held an `active` lease at
/// invocation start. A stale recovery later in the same call must never make
/// one of these removable.
pub(crate) fn pinned_active_paths(rows: &[LeaseRow]) -> BTreeSet<String> {
    rows.iter()
        .filter(|row| row.status == LEASE_STATUS_ACTIVE)
        .map(|row| {
            canonical_or_self(Path::new(&row.worktree_path))
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}
