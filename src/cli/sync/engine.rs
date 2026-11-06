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
use crate::providers::api::PhasegentError;
use crate::providers::{IssueProvider, IssueSummary};
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
    task_provider: Option<&dyn TaskProviderResolver>,
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
        task_provider,
        provider_scope,
        runner,
        storage,
        scopes,
        std::mem::take(skipped),
        mode,
        all,
    )
}

/// Resolves the pinned provider of one durable task group.
///
/// This is the only path a task group takes to a provider: the task's own
/// stored instance decides the endpoint, and a task whose instance the role
/// does not serve fails here, before any request and without a token leaving
/// the process. The lookup runs on the pass's own connection, so a pass
/// reads the very store it owns.
pub(crate) trait TaskProviderResolver {
    fn provider_for(
        &self,
        connection: &rusqlite::Connection,
        task_id: i64,
    ) -> Result<crate::providers::ProviderDispatcher, PhasegentError>;
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_sync_scopes(
    get_issue: &dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
    task_provider: Option<&dyn TaskProviderResolver>,
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
            let scope_map = crate::worktree::lease_scope::scope_map_for_repo_with_key(
                storage,
                &scope.repo_identity,
                scope.repo_key.as_deref(),
            )
            .map_err(|error| PhasegentError::request("issue sync", error.message))?;
            // Every group of this number is reconciled on its own: a durable
            // task and the legacy history are never merged, and two
            // same-number tasks on two instances never share a group.
            for group in lease_groups(issue, &fresh, &scope_map) {
                match scoped_processing(&group, rows, &fresh, &scope_map, provider_scope) {
                    ScopedProcessing::Unscoped => {}
                    ScopedProcessing::Keep(reason) => {
                        report.skipped_repos.push(super::SyncSkippedRepo {
                            repo_identity: scope.repo_identity.clone(),
                            reason,
                        });
                        continue;
                    }
                    ScopedProcessing::Provider => {
                        // The group's own provider: a durable task is read
                        // through its pinned instance, independently of the
                        // pass-wide default. An instance this role does not
                        // serve keeps the group before any request.
                        let group_provider = match resolve_group_provider(
                            &group,
                            task_provider,
                            &storage.connection,
                        ) {
                            Ok(provider) => provider,
                            Err(reason) => {
                                report.skipped_repos.push(super::SyncSkippedRepo {
                                    repo_identity: scope.repo_identity.clone(),
                                    reason: format!("issue {issue}: {reason}"),
                                });
                                continue;
                            }
                        };
                        // Read and prove the remote state before any lease or
                        // directory mutation. An open, missing, or failed read
                        // leaves the active lease exactly as it was.
                        let summary = match read_group_issue(&group_provider, get_issue, issue) {
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
                        let fresh = crate::worktree::leases::list_all_for_repo(
                            storage,
                            &scope.repo_identity,
                        )
                        .map_err(|error| PhasegentError::request("issue sync", error.message))?;
                        let fresh_scopes =
                            crate::worktree::lease_scope::scope_map_for_repo_with_key(
                                storage,
                                &scope.repo_identity,
                                scope.repo_key.as_deref(),
                            )
                            .map_err(|error| {
                                PhasegentError::request("issue sync", error.message)
                            })?;
                        match scoped_processing(&group, rows, &fresh, &fresh_scopes, provider_scope)
                        {
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
                        let lease_scope = lease_scope_for(&group).map_err(|error| {
                            PhasegentError::request("issue sync", error.message)
                        })?;
                        // A terminal lease leaves nothing to retain, but the
                        // exact-scope closure proof still runs so a shared
                        // directory can be cleaned once every associated issue is
                        // confirmed closed.
                        let verified = verified_scope_lease_ids(&group, &fresh, &fresh_scopes);
                        let released = if mode == SyncMode::Clean {
                            let closure = SyncGroupClosure::new(
                                &scope.repo_path,
                                &scope.repo_key,
                                &storage.connection,
                                get_issue,
                                task_provider,
                                provider_scope,
                            );
                            let associated =
                                directory_associated_issues(&group, &fresh, &fresh_scopes);
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
                            .map_err(|error| {
                                PhasegentError::request("issue sync", error.message)
                            })?;
                            let final_scopes =
                                crate::worktree::lease_scope::scope_map_for_repo_with_key(
                                    storage,
                                    &scope.repo_identity,
                                    scope.repo_key.as_deref(),
                                )
                                .map_err(|error| {
                                    PhasegentError::request("issue sync", error.message)
                                })?;
                            if !matches!(
                                scoped_processing(
                                    &group,
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
                                directory_associated_issues(&group, &final_rows, &final_scopes);
                            let final_ids =
                                verified_scope_lease_ids(&group, &final_rows, &final_scopes);
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
                                scope.repo_key.as_deref(),
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
                            task_provider,
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
                    .filter(|directory| {
                        directory.action == "kept" || directory.action == "would_keep"
                    })
                    .count() as u64;
                report.issues.push(entry);
            }
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

/// What one reconciliation pass does with one candidate issue.
pub(super) enum ScopedProcessing {
    /// The candidate has no scoped evidence: the legacy path handles it.
    Unscoped,
    /// The scoped evidence cannot prove a safe provider: keep with reason.
    Keep(String),
    /// The pass's provider scope matches the lease scope exactly: the
    /// provider-aware path handles it.
    Provider,
}

/// One reconciliation group inside a candidate number.
///
/// A number whose rows carry no sidecar scope is one legacy group. A number
/// whose rows carry scopes produces one group per distinct exact scope: a
/// durable task and a legacy provider/project scope are different groups and
/// are never merged. Two same-number tasks on two instances therefore never
/// share a group, and neither can be released together with the other.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LeaseGroup {
    /// The issue number the group belongs to.
    pub(super) issue: u64,
    /// The exact scope of the group, or `None` for the legacy unscoped
    /// history.
    pub(super) scope: Option<crate::worktree::LeaseScope>,
}

impl LeaseGroup {
    /// The durable task this group belongs to, when it is task-bearing.
    pub(super) fn task_id(&self) -> Option<i64> {
        self.scope.as_ref().and_then(|scope| scope.task_id())
    }
}

/// The first group of one candidate number, for the tests that classify a
/// candidate against its proven scopes.
#[cfg(test)]
pub(super) fn group_for(
    issue: u64,
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
) -> LeaseGroup {
    lease_groups(issue, all, scopes)
        .into_iter()
        .next()
        .unwrap_or(LeaseGroup { issue, scope: None })
}

/// The groups of one candidate number.
///
/// Only a row whose sidecar scope is proven contributes a group. A number
/// with no scoped row is one legacy group; a number with scoped rows has no
/// legacy group at all, because unscoped same-number history is genuine
/// legacy uncertainty and keeps the whole number before any provider read.
pub(super) fn lease_groups(
    issue: u64,
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
) -> Vec<LeaseGroup> {
    let mut groups: Vec<LeaseGroup> = Vec::new();
    for row in all.iter().filter(|row| row.issue == issue) {
        let Some(scope) = scopes.get(&row.lease_id) else {
            continue;
        };
        if groups
            .iter()
            .any(|group| group.scope.as_ref() == Some(scope))
        {
            continue;
        }
        groups.push(LeaseGroup {
            issue,
            scope: Some(scope.clone()),
        });
    }
    if groups.is_empty() {
        groups.push(LeaseGroup { issue, scope: None });
    }
    groups
}

/// Whether one row's scope names the same ownership as the group's scope.
///
/// The provider and project must agree, and a durable task must be the same
/// task on the same instance: a legacy row or a row of another task is
/// different ownership evidence and keeps the directory.
fn same_ownership(
    group_scope: &crate::worktree::LeaseScope,
    row_scope: Option<&crate::worktree::LeaseScope>,
) -> bool {
    let Some(row_scope) = row_scope else {
        return false;
    };
    if row_scope.provider() != group_scope.provider()
        || row_scope.project() != group_scope.project()
    {
        return false;
    }
    match group_scope.task_id() {
        Some(task_id) => {
            row_scope.task_id() == Some(task_id) && row_scope.instance() == group_scope.instance()
        }
        None => row_scope.task_id().is_none(),
    }
}

/// Classify one group against the fresh scoped evidence.
///
/// The affected directory set is derived from the group's own rows in `all`,
/// never from stale candidate paths, so a new same-group lease in a second
/// directory pulls that directory's full row set into the proof. Every lease
/// row on every affected canonical directory must name the same ownership as
/// the group; legacy, foreign, mixed, or unreadable evidence keeps before any
/// provider read or lease mutation. A same-number row of another task is not
/// legacy uncertainty: it is another group, and it never makes this group
/// ambiguous on its own.
pub(super) fn scoped_processing(
    group: &LeaseGroup,
    candidate: &[LeaseRow],
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
    provider_scope: Option<(&str, &str)>,
) -> ScopedProcessing {
    let issue = group.issue;
    let Some(selected) = group.scope.as_ref() else {
        // The legacy group exists only when the number has no scoped row at
        // all. An explicit Forgejo context must never infer ownership of
        // purely unscoped legacy rows from the same number: keep before any
        // provider read or lease change. Compatible legacy providers still
        // reconcile.
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
        let paths = crate::worktree::CanonicalPaths::default();
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
    };
    // Scoped ownership of this number is ambiguous when unscoped same-number
    // history exists: any such row must keep before provider dispatch or
    // retention. A row of another durable task is scoped, so it is another
    // group and not legacy uncertainty.
    if all
        .iter()
        .any(|row| row.issue == issue && !scopes.contains_key(&row.lease_id))
    {
        return ScopedProcessing::Keep(format!(
            "issue {issue}: unscoped same-number history makes the ownership ambiguous"
        ));
    }
    let affected: Vec<std::path::PathBuf> = all
        .iter()
        .filter(|row| {
            row.issue == issue
                && scopes
                    .get(&row.lease_id)
                    .is_some_and(|scope| same_ownership(selected, Some(scope)))
        })
        .map(|row| {
            crate::worktree::CanonicalPaths::default().resolve(Path::new(&row.worktree_path))
        })
        .collect();
    let paths = crate::worktree::CanonicalPaths::default();
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
        .any(|row| !same_ownership(selected, scopes.get(&row.lease_id)))
    {
        return ScopedProcessing::Keep(format!(
            "issue {issue}: scoped provider lease shares a directory with other ownership evidence"
        ));
    }
    // A durable task is dispatched through its own pinned provider, so the
    // pass-wide provider scope does not apply to it; a legacy scoped group
    // still needs the pass-wide scope to match exactly.
    if selected.task_id().is_none() {
        let Some((provider, project)) = provider_scope else {
            return ScopedProcessing::Keep(format!(
                "issue {issue}: scoped provider lease requires provider-aware cleanup"
            ));
        };
        if selected.provider() != provider || selected.project() != project {
            return ScopedProcessing::Keep(format!(
                "issue {issue}: scoped provider lease requires provider-aware cleanup"
            ));
        }
    }
    ScopedProcessing::Provider
}

/// The provider one group may read through.
///
/// A durable task is dispatched through its own pinned provider; a legacy
/// group uses the pass's own provider. An instance this role does not serve
/// fails here, before any request and without a token leaving the process.
fn resolve_group_provider(
    group: &LeaseGroup,
    task_provider: Option<&dyn TaskProviderResolver>,
    connection: &rusqlite::Connection,
) -> Result<Option<crate::providers::ProviderDispatcher>, String> {
    let Some(task_id) = group.task_id() else {
        return Ok(None);
    };
    let Some(resolver) = task_provider else {
        return Err("this pass has no task-aware provider dispatch".to_owned());
    };
    match resolver.provider_for(connection, task_id) {
        Ok(provider) => Ok(Some(provider)),
        Err(error) => Err(format!(
            "the durable task provider is unavailable: {}",
            error_message(&error)
        )),
    }
}

/// Read one group's issue through the group's own provider.
fn read_group_issue(
    group_provider: &Option<crate::providers::ProviderDispatcher>,
    get_issue: &dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
    issue: u64,
) -> Result<IssueSummary, PhasegentError> {
    match group_provider {
        Some(provider) => provider.get_issue(issue),
        None => get_issue(issue),
    }
}

/// The operator-facing message of a provider error.
fn error_message(error: &PhasegentError) -> String {
    error
        .json()
        .get("message")
        .and_then(|value| value.as_str())
        .unwrap_or("the provider read failed")
        .to_owned()
}

/// The exact-scope active lease ids of one group in the fresh complete row set.
fn verified_scope_lease_ids(
    group: &LeaseGroup,
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
) -> Vec<String> {
    let Some(selected) = group.scope.as_ref() else {
        return Vec::new();
    };
    all.iter()
        .filter(|row| {
            row.issue == group.issue
                && row.status == LEASE_STATUS_ACTIVE
                && scopes
                    .get(&row.lease_id)
                    .is_some_and(|scope| scope == selected)
        })
        .map(|row| row.lease_id.clone())
        .collect()
}

/// Every issue associated with the group's canonical directories, so the
/// pre-retention closure proof covers the whole affected physical path.
fn directory_associated_issues(
    group: &LeaseGroup,
    all: &[LeaseRow],
    scopes: &std::collections::HashMap<String, crate::worktree::LeaseScope>,
) -> Vec<u64> {
    let paths = crate::worktree::CanonicalPaths::default();
    let selected = group.scope.as_ref();
    let affected: Vec<_> = all
        .iter()
        .filter(|row| {
            row.issue == group.issue
                && selected.is_some_and(|selected| scopes.get(&row.lease_id) == Some(selected))
        })
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
        .chain(std::iter::once(group.issue))
        .collect();
    issues.sort_unstable();
    issues.dedup();
    issues
}

/// The exact lease scope of a provider-processable group. The caller has
/// already proven every involved row matches the group's ownership, so the
/// group's own scope is the exact key; a missing scope is a storage error.
fn lease_scope_for(group: &LeaseGroup) -> Result<crate::worktree::LeaseScope, WorktreeError> {
    group.scope.clone().ok_or_else(|| {
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
    task_provider: Option<&dyn TaskProviderResolver>,
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
    let closure = SyncGroupClosure::new(
        &scope.repo_path,
        &scope.repo_key,
        &storage.connection,
        get_issue,
        task_provider,
        provider_scope,
    );
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

/// Where one number is confirmed in this repository.
enum IssueRoute {
    /// The number has no durable task here: the pass's own provider answers.
    Legacy,
    /// The number's own durable task, with its pinned provider.
    Task {
        provider: Box<crate::providers::ProviderDispatcher>,
        source: String,
        project: String,
    },
}

/// The group-scoped closure resolver: every other issue of the directory is
/// confirmed through its own durable task when this repository holds a
/// membership for it, and through the pass's own provider otherwise. The
/// membership is the only proof — a number whose task lives in another
/// repository is never resolved through a global number lookup, so a foreign
/// task never authorizes a removal here — and the durable link scope must
/// agree with the resolved route before a closed answer authorizes cleanup.
struct SyncGroupClosure<'a> {
    repo_path: std::path::PathBuf,
    repo_key: Option<String>,
    connection: &'a rusqlite::Connection,
    links: crate::cli::issue_closure::GitLinkScopeReader,
    get_issue: &'a dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
    task_provider: Option<&'a dyn TaskProviderResolver>,
    provider_scope: Option<(&'a str, &'a str)>,
}

impl<'a> SyncGroupClosure<'a> {
    fn new(
        repo_path: &Path,
        repo_key: &Option<String>,
        connection: &'a rusqlite::Connection,
        get_issue: &'a dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
        task_provider: Option<&'a dyn TaskProviderResolver>,
        provider_scope: Option<(&'a str, &'a str)>,
    ) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            repo_key: repo_key.clone(),
            connection,
            links: crate::cli::issue_closure::GitLinkScopeReader::new(),
            get_issue,
            task_provider,
            provider_scope,
        }
    }

    /// The route one number is confirmed through in this repository.
    fn route(&self, issue: u64) -> Result<IssueRoute, String> {
        let tasks = match &self.repo_key {
            Some(repo_key) => {
                crate::cli::task_route::tasks_for_repo_number(self.connection, repo_key, issue)
                    .map_err(|error| {
                        format!(
                            "issue {issue}: the durable task lookup failed: {}",
                            error_message(&error)
                        )
                    })?
            }
            None => Vec::new(),
        };
        match tasks.len() {
            0 => Ok(IssueRoute::Legacy),
            1 => {
                let task = &tasks[0];
                let key = task.key();
                let Some(resolver) = self.task_provider else {
                    return Err(format!(
                        "issue {issue} has a durable task, but this pass has no task-aware dispatch"
                    ));
                };
                let provider = resolver
                    .provider_for(self.connection, task.task_id())
                    .map_err(|error| {
                        format!(
                            "issue {issue}: the durable task provider is unavailable: {}",
                            error_message(&error)
                        )
                    })?;
                Ok(IssueRoute::Task {
                    provider: Box::new(provider),
                    source: key.provider().to_owned(),
                    project: key.project().to_owned(),
                })
            }
            _ => Err(format!(
                "issue {issue} has several durable tasks in this repository; refusing to guess a provider"
            )),
        }
    }
}

impl crate::lifecycle::cleanup::closure::DirectoryIssueClosure for SyncGroupClosure<'_> {
    fn confirm_others_closed(&self, current_issue: u64, issues: &[u64]) -> Result<(), String> {
        for issue in issues {
            if *issue == current_issue {
                continue;
            }
            let route = self.route(*issue)?;
            let (route_provider, route_project) = match &route {
                IssueRoute::Legacy => {
                    let Some((provider, project)) = self.provider_scope else {
                        return Err("the sync invocation has no provider scope".to_owned());
                    };
                    (provider.to_owned(), project.to_owned())
                }
                IssueRoute::Task {
                    source, project, ..
                } => (source.clone(), project.clone()),
            };
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
                && (scope_provider != route_provider || scope_project != route_project)
            {
                return Err(format!(
                    "issue {issue}: link scope '{scope_provider}/{scope_project}' does not match the resolved provider '{route_provider}/{route_project}'"
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
            let read = match &route {
                IssueRoute::Legacy => (self.get_issue)(*issue),
                IssueRoute::Task { provider, .. } => provider.get_issue(*issue),
            };
            match read {
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
