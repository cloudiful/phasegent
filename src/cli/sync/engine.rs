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
use crate::lifecycle::AutoCleanupOutcome;
use crate::lifecycle::CleanupMode;
use crate::lifecycle::cleanup::closure::DenyOtherIssues;
use crate::lifecycle::cleanup::{CleanupContext, cleanup_closed_issue_worktrees_in_context};
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
///
/// Issue 1201 P3: a scoped group is reconciled through the provider only
/// when the pass's own provider scope matches the lease scope exactly
/// (provider and repository for Forgejo); mixed, foreign, or unproven
/// groups are still skipped with a bounded reason. The release for a
/// processed scoped group flips the exact sidecar key, so a same-number
/// legacy row is never touched.
pub(crate) fn run_sync_resolved(
    get_issue: &dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
    provider_scope: Option<(&str, &str)>,
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
        provider_scope,
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
    provider_scope: Option<(&str, &str)>,
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
            let fresh = crate::worktree::leases::list_all_for_repo(storage, &scope.repo_identity)
                .map_err(|error| PhasegentError::request("issue sync", error.message))?;
            let scope_map =
                crate::worktree::lease_scope::scope_map_for_repo(storage, &scope.repo_identity)
                    .map_err(|error| PhasegentError::request("issue sync", error.message))?;
            match scoped_processing(issue, rows, &fresh, &scope_map, provider_scope) {
                ScopedProcessing::Unscoped => {}
                ScopedProcessing::Keep(reason) => {
                    report.skipped_repos.push(super::SyncSkippedRepo {
                        repo_identity: scope.repo_identity.clone(),
                        reason,
                    });
                    continue;
                }
                ScopedProcessing::Provider => {
                    // Read and prove the remote state before any lease or
                    // directory mutation. An open, missing, or failed read
                    // leaves the active lease exactly as it was.
                    let summary = match get_issue(issue) {
                        Ok(summary) => summary,
                        Err(PhasegentError::NotFound { .. }) => {
                            report.not_found += 1;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    if !summary.state.eq_ignore_ascii_case("closed") {
                        report.not_closed += 1;
                        report.issues.push(SyncIssueReport {
                            repo_identity: scope.repo_identity.clone(),
                            issue,
                            remote_state: summary.state.clone(),
                            active_leases: rows
                                .iter()
                                .filter(|row| row.status == LEASE_STATUS_ACTIVE)
                                .count() as u64,
                            released_leases: 0,
                            cleaned: 0,
                            directories: Vec::new(),
                            warnings: vec![format!(
                                "issue {issue}: scoped lease kept because the provider reports the issue open"
                            )],
                        });
                        continue;
                    }
                    // Fresh scope/lease evidence after the provider proof:
                    // mixed, foreign, or unproven evidence keeps before the
                    // exact key is retained.
                    let fresh =
                        crate::worktree::leases::list_all_for_repo(storage, &scope.repo_identity)
                            .map_err(|error| PhasegentError::request("issue sync", error.message))?;
                    let fresh_scopes = crate::worktree::lease_scope::scope_map_for_repo(
                        storage,
                        &scope.repo_identity,
                    )
                    .map_err(|error| PhasegentError::request("issue sync", error.message))?;
                    match scoped_processing(issue, rows, &fresh, &fresh_scopes, provider_scope) {
                        ScopedProcessing::Provider => {}
                        ScopedProcessing::Keep(reason) => {
                            report.skipped_repos.push(super::SyncSkippedRepo {
                                repo_identity: scope.repo_identity.clone(),
                                reason,
                            });
                            continue;
                        }
                        ScopedProcessing::Unscoped => {
                            report.skipped_repos.push(super::SyncSkippedRepo {
                                repo_identity: scope.repo_identity.clone(),
                                reason: format!(
                                    "issue {issue}: scoped provider lease requires provider-aware cleanup"
                                ),
                            });
                            continue;
                        }
                    }
                    let lease_scope = lease_scope_for(issue, rows, &fresh_scopes, provider_scope)
                        .map_err(|error| {
                        PhasegentError::request("issue sync", error.message)
                    })?;
                    // A terminal lease leaves nothing to retain, but the
                    // exact-scope closure proof still runs so a shared
                    // directory can be cleaned once every associated issue is
                    // confirmed closed.
                    let verified =
                        verified_scope_lease_ids(issue, &fresh, &fresh_scopes, provider_scope);
                    let released = if mode == SyncMode::Clean {
                        let closure =
                            SyncScopedClosure::new(&scope.repo_path, get_issue, provider_scope);
                        let associated = directory_associated_issues(issue, &fresh, &fresh_scopes);
                        if let Err(reason) = crate::lifecycle::cleanup::closure::DirectoryIssueClosure::confirm_others_closed(
                            &closure,
                            issue,
                            &associated,
                        ) {
                            report.skipped_repos.push(super::SyncSkippedRepo {
                                repo_identity: scope.repo_identity.clone(),
                                reason: format!(
                                    "issue {issue}: scoped lease kept because {reason}"
                                ),
                            });
                            continue;
                        }
                        // Rebuild the complete scope/local proof after the
                        // remote closure checks and immediately before the
                        // mutation; a changed row set or association keeps
                        // without an unbounded retry loop.
                        let final_rows = crate::worktree::leases::list_all_for_repo(
                            storage,
                            &scope.repo_identity,
                        )
                        .map_err(|error| PhasegentError::request("issue sync", error.message))?;
                        let final_scopes = crate::worktree::lease_scope::scope_map_for_repo(
                            storage,
                            &scope.repo_identity,
                        )
                        .map_err(|error| PhasegentError::request("issue sync", error.message))?;
                        if !matches!(
                            scoped_processing(
                                issue,
                                rows,
                                &final_rows,
                                &final_scopes,
                                provider_scope
                            ),
                            ScopedProcessing::Provider
                        ) {
                            report.skipped_repos.push(super::SyncSkippedRepo {
                                repo_identity: scope.repo_identity.clone(),
                                reason: format!(
                                    "issue {issue}: scoped provider proof changed before retention"
                                ),
                            });
                            continue;
                        }
                        let final_associated =
                            directory_associated_issues(issue, &final_rows, &final_scopes);
                        let final_ids = verified_scope_lease_ids(
                            issue,
                            &final_rows,
                            &final_scopes,
                            provider_scope,
                        );
                        if final_associated != associated || final_ids != verified {
                            report.skipped_repos.push(super::SyncSkippedRepo {
                                repo_identity: scope.repo_identity.clone(),
                                reason: format!(
                                    "issue {issue}: scoped lease association changed before retention"
                                ),
                            });
                            continue;
                        }
                        let approved = crate::worktree::leases::scoped_retention_proof(
                            issue,
                            &final_rows,
                            &final_scopes,
                        );
                        run_retention_interleave_hook(storage);
                        crate::worktree::leases::retain_active_leases_for_scope_ids(
                            storage,
                            &scope.repo_identity,
                            &lease_scope,
                            &final_ids,
                            &approved,
                            super::SYNC_RELEASE_REASON,
                            crate::worktree::now_unix_secs(),
                        )
                        .map_err(|error| PhasegentError::request("issue sync", error.message))?
                    } else {
                        0
                    };
                    let entry = sync_issue_scoped(
                        runner,
                        storage,
                        &guard,
                        scope,
                        issue,
                        rows,
                        &summary.state,
                        mode,
                        released,
                        &lease_scope,
                        get_issue,
                        provider_scope,
                    )?;
                    report.checked += 1;
                    report.released_leases += entry.released_leases;
                    report.cleaned += entry.cleaned;
                    report.kept += entry
                        .directories
                        .iter()
                        .filter(|directory| {
                            directory.action == "kept" || directory.action == "would_keep"
                        })
                        .count() as u64;
                    report.issues.push(entry);
                    continue;
                }
            }
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

#[cfg(test)]
type RetentionInterleaveHook = Box<dyn FnOnce(&Storage)>;

#[cfg(test)]
thread_local! {
    static RETENTION_INTERLEAVE_HOOK: std::cell::RefCell<Option<RetentionInterleaveHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Register a one-shot test hook that runs after the final ownership proof is
/// approved and before the retention transaction opens, so a fixture can
/// exercise the real orchestration and transaction with a post-proof arrival.
#[cfg(test)]
pub(crate) fn set_retention_interleave_hook(hook: RetentionInterleaveHook) {
    RETENTION_INTERLEAVE_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
}

#[cfg(test)]
fn run_retention_interleave_hook(storage: &Storage) {
    RETENTION_INTERLEAVE_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(storage);
        }
    });
}

#[cfg(not(test))]
fn run_retention_interleave_hook(_storage: &Storage) {}

/// What the pass does with one candidate issue.
pub(super) enum ScopedProcessing {
    /// The candidate has no scoped evidence: the legacy path handles it.
    Unscoped,
    /// The scoped evidence cannot prove a safe provider: keep with reason.
    Keep(String),
    /// The pass's provider scope matches the lease scope exactly: the
    /// provider-aware path handles it.
    Provider,
}

/// Classify one candidate against the fresh scoped evidence. The affected
/// directory set is derived from every scoped row of the exact issue in
/// `all`, never from stale candidate paths, so a new same-issue lease in a
/// second directory pulls that directory's full row set into the proof. Every
/// lease row on every affected canonical directory must be scoped to the
/// selected provider/project; legacy, foreign, mixed, or unreadable evidence
/// keeps before any provider read or lease mutation.
pub(super) fn scoped_processing(
    issue: u64,
    candidate: &[LeaseRow],
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
    provider_scope: Option<(&str, &str)>,
) -> ScopedProcessing {
    let issue_scoped_rows: Vec<&LeaseRow> = all
        .iter()
        .filter(|row| row.issue == issue && scopes.contains_key(&row.lease_id))
        .collect();
    let paths = crate::worktree::CanonicalPaths::default();
    if issue_scoped_rows.is_empty() {
        // An explicit Forgejo context must never infer ownership of purely
        // unscoped legacy rows from the same number: keep before any provider
        // read or lease change. Compatible legacy providers still reconcile.
        if provider_scope.is_some_and(|(provider, _)| provider == "forgejo") {
            return ScopedProcessing::Keep(format!(
                "issue {issue}: explicit Forgejo does not own unscoped legacy history"
            ));
        }
        // A candidate with no scoped evidence follows the legacy path unless
        // its own canonical directories carry scoped history. An unrelated
        // scoped lease elsewhere in the repository must not suppress legacy
        // reconciliation.
        let own_rows: Vec<&LeaseRow> = if candidate.is_empty() {
            all.iter().filter(|row| row.issue == issue).collect()
        } else {
            candidate.iter().collect()
        };
        let own_directories: Vec<std::path::PathBuf> = own_rows
            .iter()
            .map(|row| paths.resolve(Path::new(&row.worktree_path)))
            .collect();
        let shares_scoped = all.iter().any(|row| {
            scopes.contains_key(&row.lease_id)
                && own_directories
                    .iter()
                    .any(|path| paths.resolve(Path::new(&row.worktree_path)) == *path)
        });
        if shares_scoped {
            return ScopedProcessing::Keep(format!(
                "issue {issue}: scoped provider lease shares a directory with legacy ownership"
            ));
        }
        return ScopedProcessing::Unscoped;
    }
    // Scoped ownership of this number is ambiguous regardless of path: any
    // unscoped same-number row must keep before provider dispatch or
    // retention. A purely legacy number with no scoped evidence is handled by
    // the legacy path above.
    if all
        .iter()
        .any(|row| row.issue == issue && !scopes.contains_key(&row.lease_id))
    {
        return ScopedProcessing::Keep(format!(
            "issue {issue}: unscoped same-number history makes the ownership ambiguous"
        ));
    }
    let affected: Vec<std::path::PathBuf> = issue_scoped_rows
        .iter()
        .map(|row| paths.resolve(Path::new(&row.worktree_path)))
        .collect();
    let directory_rows: Vec<&LeaseRow> = all
        .iter()
        .filter(|row| {
            affected
                .iter()
                .any(|path| paths.resolve(Path::new(&row.worktree_path)) == *path)
        })
        .collect();
    if directory_rows
        .iter()
        .any(|row| !scopes.contains_key(&row.lease_id))
    {
        return ScopedProcessing::Keep(format!(
            "issue {issue}: scoped provider lease shares a directory with unproven legacy ownership"
        ));
    }
    let Some((provider, project)) = provider_scope else {
        return ScopedProcessing::Keep(format!(
            "issue {issue}: scoped provider lease requires provider-aware cleanup"
        ));
    };
    let exact = directory_rows.iter().all(|row| {
        scopes
            .get(&row.lease_id)
            .is_some_and(|scope| scope.provider() == provider && scope.project() == project)
    });
    if exact {
        ScopedProcessing::Provider
    } else {
        ScopedProcessing::Keep(format!(
            "issue {issue}: scoped provider lease requires provider-aware cleanup"
        ))
    }
}

/// The exact-scope active lease ids of one issue in the fresh complete row set.
fn verified_scope_lease_ids(
    issue: u64,
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
    provider_scope: Option<(&str, &str)>,
) -> Vec<String> {
    let Some((provider, project)) = provider_scope else {
        return Vec::new();
    };
    all.iter()
        .filter(|row| {
            row.issue == issue
                && row.status == LEASE_STATUS_ACTIVE
                && scopes.get(&row.lease_id).is_some_and(|scope| {
                    scope.provider() == provider
                        && scope.project() == project
                        && scope.external_id() == issue
                })
        })
        .map(|row| row.lease_id.clone())
        .collect()
}

/// Every issue associated with the candidate's canonical directories, so the
/// pre-retention closure proof covers the whole affected physical path.
fn directory_associated_issues(
    issue: u64,
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
) -> Vec<u64> {
    let paths = crate::worktree::CanonicalPaths::default();
    let affected: Vec<_> = all
        .iter()
        .filter(|row| row.issue == issue && scopes.contains_key(&row.lease_id))
        .map(|row| paths.resolve(Path::new(&row.worktree_path)))
        .collect();
    let mut issues: Vec<u64> = all
        .iter()
        .filter(|row| {
            affected
                .iter()
                .any(|path| paths.resolve(Path::new(&row.worktree_path)) == *path)
        })
        .map(|row| row.issue)
        .chain(std::iter::once(issue))
        .collect();
    issues.sort_unstable();
    issues.dedup();
    issues
}

/// The exact lease scope of a provider-processable candidate. The caller
/// has already proven every involved row matches the provider scope, so
/// the first scope is the exact key; a missing proof is a storage error.
fn lease_scope_for(
    issue: u64,
    rows: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
    provider_scope: Option<(&str, &str)>,
) -> Result<crate::worktree::LeaseScope, WorktreeError> {
    let _ = (issue, provider_scope);
    rows.iter()
        .find_map(|row| scopes.get(&row.lease_id))
        .cloned()
        .ok_or_else(|| {
            WorktreeError::new(
                "storage",
                "scoped lease scope evidence disappeared during the sync pass",
            )
        })
}

/// One scoped candidate's guarded cleanup. The provider-aware
/// [`crate::cli::issue_closure::ComposedIssueStateSource`] closure proves every
/// other issue of the directory through the invocation's own provider scope;
/// an open, unknown, or mismatched issue keeps. Report mode passes
/// [`CleanupMode::Report`] and never retains, removes, or moves anything.
#[allow(clippy::too_many_arguments)]
fn sync_issue_scoped(
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    guard: &SyncOccupancyGuard<'_>,
    scope: &RepoScope,
    issue: u64,
    rows: &[LeaseRow],
    remote_state: &str,
    mode: SyncMode,
    released: u64,
    lease_scope: &crate::worktree::LeaseScope,
    get_issue: &dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
    provider_scope: Option<(&str, &str)>,
) -> Result<SyncIssueReport, PhasegentError> {
    let mut entry = SyncIssueReport {
        repo_identity: scope.repo_identity.clone(),
        issue,
        remote_state: remote_state.to_owned(),
        active_leases: rows
            .iter()
            .filter(|row| row.status == LEASE_STATUS_ACTIVE)
            .count() as u64,
        released_leases: released,
        cleaned: 0,
        directories: Vec::new(),
        warnings: Vec::new(),
    };
    let context = CleanupContext::load(storage, &scope.repo_identity)
        .map_err(|error| PhasegentError::request("issue sync", error))?;
    let closure = SyncScopedClosure::new(&scope.repo_path, get_issue, provider_scope);
    let cleanup_mode = match mode {
        SyncMode::Report => CleanupMode::Report,
        SyncMode::Clean => CleanupMode::Sync,
    };
    let outcome = cleanup_closed_issue_worktrees_in_context(
        runner,
        guard,
        cleanup_mode,
        context,
        &scope.repo_path,
        issue,
        None,
        &closure,
        Some(lease_scope),
    );
    if let AutoCleanupOutcome::Cleaned { removed, kept } = &outcome {
        entry.cleaned = *removed;
        entry.warnings.extend(kept.iter().cloned());
    }
    if let AutoCleanupOutcome::Warning { reason } = &outcome {
        entry.warnings.push(reason.clone());
    }
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
        SyncMode::Clean => {}
    }
    entry
        .directories
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entry)
}

/// The invocation-scoped closure resolver: each associated issue is read
/// through the same provider closure the pass already holds, and the durable
/// link scope must agree with the invocation's provider/project before a
/// closed answer authorizes cleanup.
struct SyncScopedClosure<'a> {
    repo_path: std::path::PathBuf,
    links: crate::cli::issue_closure::GitLinkScopeReader,
    get_issue: &'a dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
    provider_scope: Option<(&'a str, &'a str)>,
}

impl<'a> SyncScopedClosure<'a> {
    fn new(
        repo_path: &Path,
        get_issue: &'a dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
        provider_scope: Option<(&'a str, &'a str)>,
    ) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            links: crate::cli::issue_closure::GitLinkScopeReader::new(),
            get_issue,
            provider_scope,
        }
    }
}

impl crate::lifecycle::cleanup::closure::DirectoryIssueClosure for SyncScopedClosure<'_> {
    fn confirm_others_closed(&self, current_issue: u64, issues: &[u64]) -> Result<(), String> {
        let Some((provider, project)) = self.provider_scope else {
            return Err("the sync invocation has no provider scope".to_owned());
        };
        for issue in issues {
            if *issue == current_issue {
                continue;
            }
            let scopes = crate::cli::issue_closure::LinkScopeReader::scopes(
                &self.links,
                &self.repo_path,
                *issue,
            )
            .map_err(|reason| {
                format!("issue {issue}: branch-link scope could not be read: {reason}")
            })?;
            let distinct: std::collections::BTreeSet<(String, String)> = scopes
                .iter()
                .map(|scope| (scope.provider.clone(), scope.project.clone()))
                .collect();
            if distinct.len() > 1 {
                return Err(format!(
                    "issue {issue}: conflicting link scopes; refusing to guess a provider"
                ));
            }
            if let Some((scope_provider, scope_project)) = distinct.into_iter().next()
                && (scope_provider != provider || scope_project != project)
            {
                return Err(format!(
                    "issue {issue}: link scope '{scope_provider}/{scope_project}' does not match the sync provider '{provider}/{project}'"
                ));
            }
            if scopes
                .iter()
                .any(|scope| scope.external_id != issue.to_string())
            {
                return Err(format!(
                    "issue {issue}: branch-link external id does not match the lease number"
                ));
            }
            match (self.get_issue)(*issue) {
                Ok(summary) if summary.state.eq_ignore_ascii_case("closed") => {}
                Ok(_) => {
                    return Err(format!(
                        "the directory is also associated with issue {issue}, which is not confirmed closed"
                    ));
                }
                Err(PhasegentError::NotFound { .. }) => {
                    return Err(format!(
                        "the directory is also associated with issue {issue}, which no longer exists"
                    ));
                }
                Err(_) => {
                    return Err(format!(
                        "the directory is also associated with issue {issue}, whose state could not be read"
                    ));
                }
            }
        }
        Ok(())
    }

    fn refresh(&self) {
        crate::cli::issue_closure::LinkScopeReader::refresh(&self.links);
    }
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
            &DenyOtherIssues,
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
        &DenyOtherIssues,
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
