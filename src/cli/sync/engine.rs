//! The reconciliation pass itself (issue 552 Phase 2).
//!
//! [`run_sync`] walks the resolved repository scopes, asks the provider
//! for each candidate issue's state, and hands every closed issue to
//! [`sync_issue`], which converges its leases and drives the shared
//! close-time guards. `--no-clean` swaps the write steps for the
//! read-only verdict of [`report_verdict`], so both modes classify with
//! the same production code.

use std::cell::RefCell;
use std::path::Path;

use crate::infra::storage::Storage;
use crate::lifecycle::cleanup::{CleanupContext, cleanup_closed_issue_worktrees_in_context};
use crate::lifecycle::{AutoCleanupOutcome, CleanupMode};
use crate::providers::IssueSummary;
use crate::providers::api::PhasegentError;
use crate::worktree::leases::release_active_leases_for_issue_with;
use crate::worktree::opencode::ProcessOpenCodeApi;
use crate::worktree::{
    GitOutput, LEASE_STATUS_ACTIVE, LeaseRow, WorktreeError, WorktreeRunner, bounded,
};

use super::host_cache::{SyncHostCache, SyncOccupancyGuard};
use super::scopes::{IntoProviderError, RepoScope, resolve_scopes};
use super::{SYNC_RELEASE_REASON, SyncDirectoryReport, SyncIssueReport, SyncMode, SyncReport};

/// Repository scopes plus the rows read for each one, resolved once for the
/// whole invocation.
pub(crate) struct SyncSetup {
    pub(crate) storage: Storage,
    pub(crate) scopes: Vec<RepoScope>,
    pub(crate) skipped: Vec<super::SyncSkippedRepo>,
}

/// Resolve the invocation's scopes once. Every caller — direct `issue
/// sync`, the pre-subcommand pass, and the worktree list/acquire reuse —
/// builds this before it touches a provider, so the repository identity is
/// resolved once per scope instead of once per phase.
pub(crate) fn resolve_setup(
    runner: &dyn WorktreeRunner,
    storage: Storage,
    all: bool,
    cwd: &Path,
) -> Result<SyncSetup, PhasegentError> {
    match resolve_scopes(runner, &storage, all, cwd) {
        Ok((scopes, skipped)) => Ok(SyncSetup {
            storage,
            scopes,
            skipped,
        }),
        Err(error) => Err(error.into_provider_error()),
    }
}

/// Run one reconciliation pass over the already-resolved setup. Every
/// remote read goes through `get_issue`; a read failure aborts the pass so
/// a direct `issue sync` exits non-zero instead of reporting a partial
/// result as success.
///
/// The pass owns the invocation's lease store, so a release and the cleanup
/// that follows it share one connection. The cleanup rows are read after
/// the release, never from the scan snapshot, and the host memo lives for
/// the whole pass so repeated scans across directories are served locally.
pub(crate) fn run_sync_resolved(
    get_issue: &dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
    runner: &dyn WorktreeRunner,
    setup: &mut SyncSetup,
    mode: SyncMode,
    all: bool,
) -> Result<SyncReport, PhasegentError> {
    let SyncSetup {
        storage,
        scopes,
        skipped,
    } = setup;
    run_sync_scopes(
        get_issue,
        runner,
        storage,
        scopes,
        std::mem::take(skipped),
        mode,
        all,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_sync_scopes(
    get_issue: &dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    scopes: &[RepoScope],
    skipped: Vec<super::SyncSkippedRepo>,
    mode: SyncMode,
    all: bool,
) -> Result<SyncReport, PhasegentError> {
    let mut report = SyncReport::new(mode, all);
    report.skipped_repos = skipped;
    let direct = ProcessOpenCodeApi::new();
    let cache = SyncHostCache::new(&direct);
    let guard = SyncOccupancyGuard::new(&cache, &direct);
    for scope in scopes.iter() {
        for (issue, rows) in &scope.candidates {
            let issue = *issue;
            report.checked += 1;
            let summary = match get_issue(issue) {
                Ok(summary) => summary,
                // A remote issue that no longer exists is not "closed":
                // record it and keep going so one stale lease row cannot
                // wedge the whole pass.
                Err(PhasegentError::NotFound { .. }) => {
                    report.not_found += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !summary.state.eq_ignore_ascii_case("closed") {
                report.not_closed += 1;
                continue;
            }
            let entry = sync_issue(
                runner,
                storage,
                &guard,
                scope,
                issue,
                rows,
                &summary.state,
                mode,
            )?;
            report.released_leases += entry.released_leases;
            report.cleaned += entry.cleaned;
            report.kept += entry
                .directories
                .iter()
                .filter(|directory| directory.action == "kept" || directory.action == "would_keep")
                .count() as u64;
            report.issues.push(entry);
        }
    }
    Ok(report)
}

/// Group the rows of one repository into candidate issues: only rows whose
/// worktree directory still exists are residue worth reconciling, and the
/// issue order is deterministic so two runs report identically.
pub(super) fn candidate_issues(rows: Vec<LeaseRow>) -> Vec<(u64, Vec<LeaseRow>)> {
    let mut candidates: Vec<(u64, Vec<LeaseRow>)> = Vec::new();
    for row in rows {
        if !Path::new(&row.worktree_path).exists() {
            continue;
        }
        match candidates.iter_mut().find(|(issue, _)| *issue == row.issue) {
            Some((_, grouped)) => grouped.push(row),
            None => candidates.push((row.issue, vec![row])),
        }
    }
    candidates.sort_by_key(|(issue, _)| *issue);
    candidates
}

#[allow(clippy::too_many_arguments)]
fn sync_issue(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    guard: &SyncOccupancyGuard<'_>,
    scope: &RepoScope,
    issue: u64,
    rows: &[LeaseRow],
    remote_state: &str,
    mode: SyncMode,
) -> Result<SyncIssueReport, PhasegentError> {
    let active_leases = rows
        .iter()
        .filter(|row| row.status == LEASE_STATUS_ACTIVE)
        .count() as u64;
    let mut entry = SyncIssueReport {
        repo_identity: scope.repo_identity.clone(),
        issue,
        remote_state: remote_state.to_owned(),
        active_leases,
        released_leases: 0,
        cleaned: 0,
        directories: Vec::new(),
        warnings: Vec::new(),
    };
    match mode {
        SyncMode::Report => {
            for row in rows {
                let (action, reason) = report_verdict(runner, &scope.repo_path, issue, row);
                entry.directories.push(SyncDirectoryReport {
                    path: row.worktree_path.clone(),
                    action,
                    reason,
                });
            }
        }
        SyncMode::Clean => clean_issue(runner, storage, guard, scope, issue, rows, &mut entry)?,
    }
    entry
        .directories
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entry)
}

/// Clean-mode half of [`sync_issue`]: converge the issue's leases with the
/// close chain's primitive, then let the shared guards decide per
/// directory. A guard pass that could not resolve its context removes
/// nothing and is reported as keeping every candidate.
fn clean_issue(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    guard: &SyncOccupancyGuard<'_>,
    scope: &RepoScope,
    issue: u64,
    rows: &[LeaseRow],
    entry: &mut SyncIssueReport,
) -> Result<(), PhasegentError> {
    entry.released_leases = release_active_leases_for_issue_with(
        storage,
        &scope.repo_identity,
        issue,
        SYNC_RELEASE_REASON,
    )
    .map_err(|error| PhasegentError::request("issue sync", error.message))?;
    // Issue 747 P1: the pass checks OpenCode occupancy but never moves a
    // session. A directory whose session a close already relocated is
    // found free here and becomes eligible; one whose session is still
    // hosted there stays until a later pass sees it leave.
    let outcome = match CleanupContext::load(storage, &scope.repo_identity) {
        Ok(context) => cleanup_closed_issue_worktrees_in_context(
            runner,
            guard,
            CleanupMode::Sync,
            context,
            &scope.repo_path,
            issue,
            None,
        ),
        Err(message) => AutoCleanupOutcome::Warning {
            reason: format!(
                "issue {issue} closed; could not read worktree leases for cleanup: {}",
                bounded(&message)
            ),
        },
    };
    let kept = match &outcome {
        AutoCleanupOutcome::Warning { reason } => {
            let reason = bounded(reason);
            entry.warnings.push(reason.clone());
            rows.iter()
                .map(|row| format!("{reason}: {}", row.worktree_path))
                .collect::<Vec<_>>()
        }
        _ => kept_entries(&outcome),
    };
    for row in rows {
        match kept
            .iter()
            .find(|entry| entry.ends_with(&kept_entry_suffix(&row.worktree_path)))
        {
            Some(reason) => entry.directories.push(SyncDirectoryReport {
                path: row.worktree_path.clone(),
                action: "kept".to_owned(),
                reason: Some(reason.clone()),
            }),
            // The shared guard returns one entry per kept directory; a
            // surviving directory without one can only mean the pass did
            // not classify it, so it is still kept.
            None if Path::new(&row.worktree_path).exists() => {
                entry.directories.push(SyncDirectoryReport {
                    path: row.worktree_path.clone(),
                    action: "kept".to_owned(),
                    reason: Some(unclassified_keep_reason(issue, &row.worktree_path)),
                });
            }
            None => {
                entry.cleaned += 1;
                entry.directories.push(SyncDirectoryReport {
                    path: row.worktree_path.clone(),
                    action: "cleaned".to_owned(),
                    reason: None,
                });
            }
        }
    }
    Ok(())
}

/// Read-only verdict for one directory in report mode.
///
/// Report mode must not write, and the shared guard implementation lives in
/// [`crate::lifecycle::cleanup_closed_issue_worktrees_with`] (no dry-run
/// switch). The verdict is therefore produced by driving that same
/// function through a runner that answers the single mutating invocation
/// (`git worktree remove`) with success and records its target, so every
/// predicate (main checkout, foreign active lease, cleanliness) is still
/// evaluated by the shared code instead of a second implementation.
///
/// [`crate::lifecycle::CleanupMode::Report`] additionally forbids the
/// OpenCode conversation itself: an associated candidate is reported
/// `would_keep` without a single API call, which is the conservative
/// direction for a pass that promises to change nothing.
///
/// The clean pass flips this issue's `active` leases before it cleans, so
/// the verdict attributes the candidate row's own session as the
/// convergent owner: the guards then see exactly the state the clean pass
/// reaches for this directory while another session's or issue's active
/// lease still keeps its directory.
fn report_verdict(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    row: &LeaseRow,
) -> (String, Option<String>) {
    let dry_run = DryRunRunner::new(runner);
    let outcome = crate::lifecycle::cleanup_closed_issue_worktrees_with(
        &dry_run,
        &crate::worktree::opencode::ProcessOpenCodeApi::new(),
        crate::lifecycle::CleanupMode::Report,
        repo_path,
        issue,
        Some(row.session.as_str()),
    );
    if dry_run.removed(&row.worktree_path) {
        return ("would_clean".to_owned(), None);
    }
    match &outcome {
        AutoCleanupOutcome::Cleaned { kept, .. } => {
            let reason = kept
                .iter()
                .find(|entry| entry.ends_with(&kept_entry_suffix(&row.worktree_path)))
                .cloned()
                .unwrap_or_else(|| unclassified_keep_reason(issue, &row.worktree_path));
            ("would_keep".to_owned(), Some(reason))
        }
        AutoCleanupOutcome::Warning { reason } => ("would_keep".to_owned(), Some(bounded(reason))),
        AutoCleanupOutcome::Noop => (
            "would_keep".to_owned(),
            Some(format!(
                "issue {issue} synced; kept worktree (no cleanup candidate): {}",
                row.worktree_path
            )),
        ),
    }
}

fn unclassified_keep_reason(issue: u64, directory: &str) -> String {
    format!("issue {issue} synced; kept worktree (the cleanup guards kept it): {directory}")
}

/// Every kept-directory entry the shared cleanup returns ends with
/// `": "` and the lease row's `worktree_path` verbatim, so matching that
/// suffix addresses one directory exactly instead of matching a path that
/// merely ends with the same characters.
fn kept_entry_suffix(directory: &str) -> String {
    format!(": {directory}")
}

/// Wrapper that delegates every probe to the real runner and turns the one
/// mutating invocation the guarded cleanup performs into a recorded no-op,
/// so a report pass classifies with the production guards without writing.
struct DryRunRunner<'a> {
    inner: &'a dyn WorktreeRunner,
    removals: RefCell<Vec<String>>,
}

impl<'a> DryRunRunner<'a> {
    fn new(inner: &'a dyn WorktreeRunner) -> Self {
        Self {
            inner,
            removals: RefCell::new(Vec::new()),
        }
    }

    /// True when the cleanup decided to remove `directory`. The cleanup
    /// passes the lease row's `worktree_path` verbatim as the removal
    /// target, so the recorded argument is comparable as text.
    fn removed(&self, directory: &str) -> bool {
        self.removals
            .borrow()
            .iter()
            .any(|recorded| recorded == directory)
    }
}

impl WorktreeRunner for DryRunRunner<'_> {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError> {
        if args.len() >= 3 && args[0] == "worktree" && args[1] == "remove" {
            self.removals.borrow_mut().push(args[2].to_owned());
            return Ok(GitOutput {
                status: 0,
                stdout: String::new(),
            });
        }
        self.inner.run(args, workdir)
    }
}

fn kept_entries(outcome: &AutoCleanupOutcome) -> Vec<String> {
    match outcome {
        AutoCleanupOutcome::Cleaned { kept, .. } => kept.clone(),
        AutoCleanupOutcome::Warning { reason } => vec![bounded(reason)],
        AutoCleanupOutcome::Noop => Vec::new(),
    }
}
