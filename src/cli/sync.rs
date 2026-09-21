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
#[path = "sync/scopes.rs"]
mod scopes;
use engine::candidate_issues;
pub(crate) use engine::run_sync;
use scopes::{default_provider, open_lease_storage, resolve_scopes};

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
    if role != Role::Orchestrator {
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
    let request = SyncRequest {
        all,
        mode,
        cwd: &cwd,
    };
    match run_sync(&provider, &ProcessWorktreeRunner::new(), &storage, request) {
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
pub(crate) fn taxi_sync(role: Role, repo: Option<&str>) -> Vec<String> {
    let storage = match open_lease_storage() {
        Ok(storage) => storage,
        Err(message) => return vec![bounded(&format!("sync skipped: {message}"))],
    };
    let cwd = match repo {
        Some(raw) => PathBuf::from(raw),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let runner = ProcessWorktreeRunner::new();
    // No candidate directory means nothing to reconcile: skip before any
    // provider resolution so an idle invocation stays silent and offline.
    match resolve_scopes(&runner, &storage, false, &cwd) {
        Ok((scopes, _)) => {
            if scopes
                .iter()
                .all(|scope| candidate_issues(&scope.rows).is_empty())
            {
                return Vec::new();
            }
        }
        Err(error) => return vec![bounded(&format!("sync skipped: {}", error.message))],
    }
    let provider = match default_provider(role) {
        Ok(provider) => provider,
        Err(error) => return vec![bounded(&format!("sync skipped: {}", error_json(&error)))],
    };
    let request = SyncRequest {
        all: false,
        mode: SyncMode::Clean,
        cwd: &cwd,
    };
    match run_sync(&provider, &runner, &storage, request) {
        Ok(report) => report.taxi_warnings(),
        Err(error) => vec![bounded(&format!("sync skipped: {}", error_json(&error)))],
    }
}

fn storage_error(message: &str) -> i32 {
    structured_error(
        serde_json::json!({"kind": "storage", "operation": "issue sync", "message": message}),
        1,
    )
}

fn error_json(error: &crate::providers::api::ForgejoError) -> String {
    error.json().to_string()
}
