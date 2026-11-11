//! The reconciliation pass itself.
//!
//! [`run_sync_resolved`] walks the resolved repository scopes, asks the
//! provider for each registered issue's state, and hands every closed issue to
//! the shared close-time cleanup guards. `--no-clean` classifies the same
//! candidates and writes nothing.

use std::collections::BTreeSet;

use crate::infra::storage::Storage;
use crate::lifecycle::AutoCleanupOutcome;
use crate::lifecycle::CleanupMode;
use crate::lifecycle::cleanup::candidate::{self, DirectOccupancy};
use crate::lifecycle::cleanup::{
    CleanupContext, associated_sessions, cleanup_closed_issue_worktrees_in_context,
};
use crate::providers::api::PhasegentError;
use crate::providers::{IssueProvider, IssueSummary};
use crate::worktree::opencode::ProcessOpenCodeApi;
use crate::worktree::{WorktreeError, WorktreeRunner};

use super::scopes::{IntoProviderError, RepoScope, resolve_scopes};
use super::{SyncDirectoryReport, SyncIssueReport, SyncMode, SyncReport};

pub(crate) struct SyncSetup {
    pub(crate) storage: Storage,
    pub(crate) scopes: Vec<RepoScope>,
    pub(crate) skipped: Vec<super::SyncSkippedRepo>,
}

pub(crate) fn resolve_setup(
    runner: &dyn WorktreeRunner,
    storage: Storage,
    all: bool,
    cwd: &std::path::Path,
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

/// The provider facts one sync pass carries: its resolved `(provider,
/// project)` scope, whether the selection keeps legacy numeric associations,
/// and the role/context used to build closure evidence.
pub(crate) struct SyncProviderScope<'a> {
    pub scope: Option<(&'a str, &'a str)>,
    pub legacy_numeric_leases: bool,
    pub role: crate::policy::Role,
    pub context: &'a crate::cli::provider_context::ProviderContext,
}

pub(crate) fn run_sync_resolved(
    get_issue: &dyn Fn(u64) -> Result<IssueSummary, PhasegentError>,
    task_provider: Option<&dyn TaskProviderResolver>,
    provider: SyncProviderScope<'_>,
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
        provider,
        runner,
        storage,
        scopes,
        std::mem::take(skipped),
        mode,
        all,
    )
}

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
    provider: SyncProviderScope<'_>,
    runner: &dyn WorktreeRunner,
    storage: &Storage,
    scopes: &[RepoScope],
    skipped: Vec<super::SyncSkippedRepo>,
    mode: SyncMode,
    all: bool,
) -> Result<SyncReport, PhasegentError> {
    let mut report = SyncReport::new(mode, all);
    report.skipped_repos = skipped;
    let api = ProcessOpenCodeApi::new();
    for scope in scopes {
        let issues: BTreeSet<u64> = scope
            .records
            .iter()
            .flat_map(|record| record.issue_numbers())
            .collect();
        for issue in issues {
            report.checked += 1;
            let task = scope
                .records
                .iter()
                .flat_map(|record| record.tasks.iter())
                .find(|task| task.issue == issue);
            let summary = match task {
                Some(task) => match task_provider {
                    Some(resolver) => {
                        let dispatcher =
                            resolver.provider_for(&storage.connection, task.task_id)?;
                        match dispatcher.get_issue(issue) {
                            Ok(summary) => summary,
                            Err(PhasegentError::NotFound { .. }) => {
                                report.not_found += 1;
                                continue;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    None => match get_issue(issue) {
                        Ok(summary) => summary,
                        Err(PhasegentError::NotFound { .. }) => {
                            report.not_found += 1;
                            continue;
                        }
                        Err(error) => return Err(error),
                    },
                },
                None => match get_issue(issue) {
                    Ok(summary) => summary,
                    Err(PhasegentError::NotFound { .. }) => {
                        report.not_found += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                },
            };
            if !summary.state.eq_ignore_ascii_case("closed") {
                report.not_closed += 1;
                continue;
            }
            let entry = if mode == SyncMode::Clean {
                let context = match CleanupContext::load(storage, &scope.repo_identity) {
                    Ok(context) => context,
                    Err(reason) => {
                        report.skipped_repos.push(super::SyncSkippedRepo {
                            repo_identity: scope.repo_identity.clone(),
                            reason: format!("issue {issue}: {reason}"),
                        });
                        continue;
                    }
                };
                let closure = crate::cli::issue_closure::CurrentIssueClosure::new_task_aware(
                    &scope.repo_path,
                    provider.role,
                    provider.context.clone(),
                );
                let outcome = cleanup_closed_issue_worktrees_in_context(
                    runner,
                    &DirectOccupancy(&api),
                    CleanupMode::Sync,
                    Some(storage),
                    context,
                    &scope.repo_path,
                    issue,
                    None,
                    &closure,
                    allowed_scope_for(task, issue, provider.scope, provider.legacy_numeric_leases)
                        .as_ref(),
                );
                issue_entry(&scope.repo_identity, issue, &summary.state, outcome)
            } else {
                report_entry(
                    runner,
                    &scope.repo_identity,
                    &scope.records,
                    issue,
                    &summary.state,
                    allowed_scope_for(task, issue, provider.scope, provider.legacy_numeric_leases)
                        .as_ref(),
                )
            };
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

fn report_entry(
    runner: &dyn WorktreeRunner,
    repo_identity: &str,
    records: &[crate::worktree::DirectoryRecord],
    issue: u64,
    remote_state: &str,
    allowed_scope: Option<&crate::worktree::WorktreeScope>,
) -> SyncIssueReport {
    let context = CleanupContext {
        identity: repo_identity.to_owned(),
        records: records.to_vec(),
    };
    let mut directories = Vec::new();
    for record in records.iter().filter(|record| record.has_issue(issue)) {
        let directory = std::path::PathBuf::from(&record.worktree_path);
        if !directory.exists() {
            continue;
        }
        let verdict = if !context.directory_scope_allowed(&directory, allowed_scope) {
            Some("scoped provider association requires provider-aware cleanup".to_owned())
        } else {
            candidate::local_guard(runner, repo_identity, &directory).protected_reason
        };
        let sessions = associated_sessions(records, &directory);
        let verdict = verdict.or_else(|| {
            (!sessions.is_empty()).then(|| {
                format!(
                    "an OpenCode session association points here ({}); occupancy is not probed in report mode",
                    sessions.join(", ")
                )
            })
        });
        directories.push(match verdict {
            Some(reason) => SyncDirectoryReport {
                path: record.worktree_path.clone(),
                action: "would_keep".to_owned(),
                reason: Some(reason),
            },
            None => SyncDirectoryReport {
                path: record.worktree_path.clone(),
                action: "would_clean".to_owned(),
                reason: None,
            },
        });
    }
    SyncIssueReport {
        repo_identity: repo_identity.to_owned(),
        issue,
        remote_state: remote_state.to_owned(),
        cleaned: 0,
        directories,
        warnings: Vec::new(),
    }
}

fn allowed_scope_for(
    task: Option<&crate::worktree::DirectoryTask>,
    issue: u64,
    provider_scope: Option<(&str, &str)>,
    legacy_numeric_leases: bool,
) -> Option<crate::worktree::WorktreeScope> {
    if let Some(task) = task {
        let instance = task.instance.as_deref().unwrap_or_default();
        return crate::worktree::WorktreeScope::for_task(
            task.provider.as_deref().unwrap_or_default(),
            task.project.as_deref().unwrap_or_default(),
            task.issue,
            task.task_id,
            instance,
        )
        .ok();
    }
    if legacy_numeric_leases {
        return None;
    }
    let (provider, project) = provider_scope?;
    crate::worktree::WorktreeScope::new(provider, project, issue).ok()
}

fn issue_entry(
    repo_identity: &str,
    issue: u64,
    remote_state: &str,
    outcome: AutoCleanupOutcome,
) -> SyncIssueReport {
    match outcome {
        AutoCleanupOutcome::Noop => SyncIssueReport {
            repo_identity: repo_identity.to_owned(),
            issue,
            remote_state: remote_state.to_owned(),
            cleaned: 0,
            directories: Vec::new(),
            warnings: Vec::new(),
        },
        AutoCleanupOutcome::Cleaned { removed, kept } => SyncIssueReport {
            repo_identity: repo_identity.to_owned(),
            issue,
            remote_state: remote_state.to_owned(),
            cleaned: removed,
            directories: kept
                .into_iter()
                .map(|warning| SyncDirectoryReport {
                    path: warning.clone(),
                    action: "kept".to_owned(),
                    reason: Some(warning),
                })
                .collect(),
            warnings: Vec::new(),
        },
        AutoCleanupOutcome::Warning { reason } => SyncIssueReport {
            repo_identity: repo_identity.to_owned(),
            issue,
            remote_state: remote_state.to_owned(),
            cleaned: 0,
            directories: Vec::new(),
            warnings: vec![reason],
        },
    }
}

#[allow(dead_code)]
fn worktree_error(error: WorktreeError) -> PhasegentError {
    PhasegentError::request("issue sync", error.message)
}
