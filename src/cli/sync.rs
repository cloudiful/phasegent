//! `issue sync` — remote/local reconciliation (issue 552 Phase 2).
//!
//! The pass answers one question per local worktree lease: the provider's
//! issue is already closed, so is the local residue still needed? A
//! remotely closed issue converges exactly the way `issue close` converges
//! it locally:
//!
//! 1. its `active` leases in the scanned repository flip to `retained`
//!    through [`crate::worktree::release_active_leases_for_issue`] — the
//!    same primitive the close chain reaches through
//!    `lifecycle::release_closed_issue_leases`;
//! 2. each of its worktree directories then runs the shared
//!    [`crate::lifecycle::cleanup_closed_issue_worktrees`] guards (clean,
//!    no foreign `active` lease on the directory, never the main
//!    checkout), so the deletion rule is never re-implemented here.
//!
//! Branches are never deleted, lease rows are never deleted, and the
//! pass is best-effort per issue: a directory a guard keeps stays on disk
//! with the guard's reason in the report.
//!
//! ## Scope and switches
//!
//! * default — the repository of the current working directory;
//! * `--all` — every repository identity recorded in the lease table,
//!   resolved through its main checkout;
//! * `--no-clean` — report mode: the same candidates and the same guard
//!   verdicts, but nothing is written (no lease flip, no removal).
//!
//! ## Failure semantics
//!
//! A remote read failure is a hard error for a direct `issue sync`
//! (non-zero exit, structured error on stderr) and a bounded stderr
//! warning for the `worktree acquire|list|prune` reconciliation pass,
//! which never blocks the subcommand it runs before.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::policy::Role;
use crate::providers::ProviderKind;
use crate::worktree::{ProcessWorktreeRunner, bounded};

#[path = "sync/engine.rs"]
mod engine;
#[path = "sync/host_cache.rs"]
mod host_cache;
#[path = "sync/scopes.rs"]
mod scopes;
#[cfg(test)]
#[path = "sync/tests_reuse.rs"]
mod tests_reuse;
use crate::providers::IssueProvider;
pub(crate) use engine::{SyncSetup, resolve_setup, run_sync_resolved};
use scopes::{default_provider, open_lease_storage};

/// Lease `release_reason` recorded when the reconciliation converges an
/// issue the provider already closed.
const SYNC_RELEASE_REASON: &str = "issue closed on the remote (issue sync)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncMode {
    /// Flip the closed issue's active leases and run the guarded cleanup.
    Clean,
    /// Classify the same candidates and write nothing.
    Report,
}

impl SyncMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Report => "report",
        }
    }
}

/// One directory verdict of the pass. `would_*` actions belong to report
/// mode; `cleaned` / `kept` are the clean-mode outcomes.
#[derive(Debug, Serialize)]
pub(crate) struct SyncDirectoryReport {
    pub path: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SyncIssueReport {
    pub repo_identity: String,
    pub issue: u64,
    pub remote_state: String,
    /// `active` lease rows the clean pass flips to `retained` (the count
    /// the report mode would flip; report mode writes nothing).
    pub active_leases: u64,
    pub released_leases: u64,
    pub cleaned: u64,
    pub directories: Vec<SyncDirectoryReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SyncSkippedRepo {
    pub repo_identity: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SyncReport {
    pub mode: &'static str,
    pub all: bool,
    pub checked: usize,
    pub not_closed: usize,
    pub not_found: usize,
    pub released_leases: u64,
    pub cleaned: u64,
    pub kept: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_repos: Vec<SyncSkippedRepo>,
    pub issues: Vec<SyncIssueReport>,
}

impl SyncReport {
    fn new(mode: SyncMode, all: bool) -> Self {
        Self {
            mode: mode.as_str(),
            all,
            checked: 0,
            not_closed: 0,
            not_found: 0,
            released_leases: 0,
            cleaned: 0,
            kept: 0,
            skipped_repos: Vec::new(),
            issues: Vec::new(),
        }
    }

    /// One bounded stderr line per issue the pass actually changed, plus
    /// the guards' own keep reasons for those issues. Issues nothing
    /// happened to stay silent so a repeated subcommand does not repeat
    /// the same warning forever.
    fn taxi_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for issue in &self.issues {
            if issue.released_leases == 0 && issue.cleaned == 0 {
                continue;
            }
            warnings.push(bounded(&format!(
                "issue {} closed on the remote; released {} lease(s) and removed {} worktree director{}",
                issue.issue,
                issue.released_leases,
                issue.cleaned,
                if issue.cleaned == 1 { "y" } else { "ies" }
            )));
            for directory in &issue.directories {
                if directory.action == "kept"
                    && let Some(reason) = &directory.reason
                {
                    warnings.push(bounded(reason));
                }
            }
            for warning in &issue.warnings {
                warnings.push(bounded(warning));
            }
        }
        warnings
    }
}

pub(crate) struct SyncRequest<'a> {
    pub all: bool,
    pub mode: SyncMode,
    pub cwd: &'a Path,
}

#[allow(dead_code)]
pub(crate) fn run_sync(
    provider: &crate::providers::ProviderDispatcher,
    runner: &dyn crate::worktree::WorktreeRunner,
    storage: &crate::infra::storage::Storage,
    request: SyncRequest<'_>,
) -> Result<SyncReport, crate::providers::api::PhasegentError> {
    use scopes::IntoProviderError;
    let (scopes, skipped) = scopes::resolve_scopes(runner, storage, request.all, request.cwd)
        .map_err(|error| error.into_provider_error())?;
    engine::run_sync_scopes(
        &|issue| provider.get_issue(issue),
        runner,
        storage,
        &scopes,
        skipped,
        request.mode,
        request.all,
    )
}

#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_sync(
    role: Role,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    all: bool,
    no_clean: bool,
) -> i32 {
    let caller = crate::policy::CallerContext::Agent(role);
    execute_sync_with_caller(
        caller,
        role,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
        all,
        no_clean,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_sync_with_caller(
    caller: crate::policy::CallerContext,
    role: Role,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    all: bool,
    no_clean: bool,
) -> i32 {
    if !caller.is_orchestrator_authorized() {
        return crate::cli::worktree::permission_error(role, "issue sync");
    }
    let provider =
        match crate::providers::config::resolve_kind(role, provider_kind).and_then(|kind| {
            crate::cli::provider_for(
                role,
                Some(kind),
                api_base,
                repository,
                project_id,
                close_status_id,
            )
        }) {
            Ok(provider) => provider,
            Err(error) => return crate::cli::provider_error(error),
        };
    let storage = match open_lease_storage() {
        Ok(storage) => storage,
        Err(message) => return storage_error(&message),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mode = if no_clean {
        SyncMode::Report
    } else {
        SyncMode::Clean
    };
    let runner = ProcessWorktreeRunner::new();
    let mut setup = match resolve_setup(&runner, storage, all, &cwd) {
        Ok(setup) => setup,
        Err(error) => return crate::cli::provider_error(error),
    };
    crate::output::progress::stage("issue sync: reconciling the worktree leases");
    match run_sync_resolved(
        &|issue| provider.get_issue(issue),
        &runner,
        &mut setup,
        mode,
        all,
    ) {
        Ok(report) => print_json(&report),
        Err(error) => crate::cli::provider_error(error),
    }
}

/// Pre-subcommand reconciliation for `worktree acquire|list|prune` (issue
/// 552 Phase 2). Never writes stdout: the caller forwards the returned
/// lines to stderr, so the subcommand's own envelope is byte-identical
/// whether or not the pass ran. `repo` is the subcommand's `--repo`
/// checkout (the current directory when it was not supplied). Returns
/// nothing when there is no local residue to reconcile, and every failure
/// is a warning instead of an error.
#[allow(dead_code)]
pub(crate) fn taxi_sync(role: Role, repo: Option<&str>) -> Vec<String> {
    taxi_sync_seeded(role, repo).0
}

#[allow(dead_code)]
pub(crate) fn taxi_sync_seeded(role: Role, repo: Option<&str>) -> (Vec<String>, Option<SyncSetup>) {
    taxi_sync_with_runner(role, repo, &ProcessWorktreeRunner::new())
}

pub(crate) fn taxi_sync_with_runner(
    role: Role,
    repo: Option<&str>,
    runner: &dyn crate::worktree::WorktreeRunner,
) -> (Vec<String>, Option<SyncSetup>) {
    let storage = match open_lease_storage() {
        Ok(storage) => storage,
        Err(message) => return (vec![bounded(&format!("sync skipped: {message}"))], None),
    };
    let cwd = match repo {
        Some(raw) => PathBuf::from(raw),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    taxi_sync_in(runner, storage, &cwd, || {
        let provider = default_provider(role)?;
        Ok(move |issue| provider.get_issue(issue))
    })
}

pub(crate) fn taxi_sync_in<G, R>(
    runner: &dyn crate::worktree::WorktreeRunner,
    storage: crate::infra::storage::Storage,
    cwd: &Path,
    resolve_provider: R,
) -> (Vec<String>, Option<SyncSetup>)
where
    G: Fn(u64) -> Result<crate::providers::IssueSummary, crate::providers::api::PhasegentError>,
    R: FnOnce() -> Result<G, crate::providers::api::PhasegentError>,
{
    let mut setup = match resolve_setup(runner, storage, false, cwd) {
        Ok(setup) => setup,
        Err(error) => {
            let message = match error {
                crate::providers::api::PhasegentError::Request { message, .. } => message,
                other => error_json(&other),
            };
            return (vec![bounded(&format!("sync skipped: {message}"))], None);
        }
    };
    if setup.scopes.iter().all(|scope| scope.candidates.is_empty()) {
        return (Vec::new(), Some(setup));
    }
    let get_issue = match resolve_provider() {
        Ok(get_issue) => get_issue,
        Err(error) => {
            return (
                vec![bounded(&format!("sync skipped: {}", error_json(&error)))],
                Some(setup),
            );
        }
    };
    let warnings = match run_sync_resolved(&get_issue, runner, &mut setup, SyncMode::Clean, false) {
        Ok(report) => report.taxi_warnings(),
        Err(error) => vec![bounded(&format!("sync skipped: {}", error_json(&error)))],
    };
    (warnings, Some(setup))
}

fn storage_error(message: &str) -> i32 {
    structured_error(
        serde_json::json!({"kind": "storage", "operation": "issue sync", "message": message}),
        1,
    )
}

fn error_json(error: &crate::providers::api::PhasegentError) -> String {
    error.json().to_string()
}
