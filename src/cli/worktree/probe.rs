//! Executor for the read-only `worktree probe` subcommand (issue 595).
//!
//! The command answers one question without side effects: given
//! `--path PATH`, or `--issue N [--session S]`, or neither (the current
//! checkout), what is on disk? It never calls a provider, never writes a
//! lease, never syncs, and never repairs anything. A Git or filesystem
//! failure is reported inside the bounded JSON envelope rather than as a
//! hard error; only an unusable storage or repo identity is a structured
//! error.
//!
//! The role gate lives in the parent module's dispatcher, not here.

use std::path::PathBuf;

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::worktree::leases::ensure_schema;
use crate::worktree::probe::probe_path;
use crate::worktree::{
    LeaseRow, ProbeError, ProbeFacts, ProcessWorktreeRunner, WorktreeError,
    find_active_lease_for_probe, repo_identity,
};

use super::open_storage;

/// Lease summary attached to a probe that resolved `--issue`.
#[derive(Debug, Serialize)]
pub(crate) struct ProbeLeaseJson {
    pub lease_id: String,
    pub repo_identity: String,
    pub issue: u64,
    pub session: String,
    pub status: String,
    pub branch: String,
    pub worktree_path: String,
}

impl From<LeaseRow> for ProbeLeaseJson {
    fn from(row: LeaseRow) -> Self {
        Self {
            lease_id: row.lease_id,
            repo_identity: row.repo_identity,
            issue: row.issue,
            session: row.session,
            status: row.status,
            branch: row.branch,
            worktree_path: row.worktree_path,
        }
    }
}

/// One bounded probe failure in the JSON envelope.
#[derive(Debug, Serialize)]
pub(crate) struct ProbeErrorJson {
    pub kind: String,
    pub message: String,
}

impl From<ProbeError> for ProbeErrorJson {
    fn from(error: ProbeError) -> Self {
        Self {
            kind: error.kind.to_owned(),
            message: error.message,
        }
    }
}

/// Stable, bounded envelope for one probe. Field order is part of the
/// contract for downstream parsers.
#[derive(Debug, Serialize)]
pub(crate) struct ProbeJson {
    pub resolved: bool,
    pub path: Option<String>,
    pub exists: bool,
    pub is_git_worktree: bool,
    /// `true` clean, `false` dirty, `null` unknown (probe failed or the
    /// path is not a Git work tree).
    pub clean: Option<bool>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub is_main_checkout: Option<bool>,
    pub lease: Option<ProbeLeaseJson>,
    pub errors: Vec<ProbeErrorJson>,
}

impl ProbeJson {
    /// The stable empty result for an `--issue` selector that matched no
    /// active lease: no path is guessed.
    fn unresolved() -> Self {
        Self {
            resolved: false,
            path: None,
            exists: false,
            is_git_worktree: false,
            clean: None,
            branch: None,
            head: None,
            is_main_checkout: None,
            lease: None,
            errors: Vec::new(),
        }
    }

    /// Assemble the envelope for a resolved path plus its read-only
    /// facts and (optionally) the lease the selector matched.
    fn resolved_at(path: String, facts: ProbeFacts, lease: Option<LeaseRow>) -> Self {
        Self {
            resolved: true,
            path: Some(path),
            exists: facts.exists,
            is_git_worktree: facts.is_git_worktree,
            clean: facts.clean,
            branch: facts.branch,
            head: facts.head,
            is_main_checkout: facts.is_main_checkout,
            lease: lease.map(ProbeLeaseJson::from),
            errors: facts.errors.into_iter().map(ProbeErrorJson::from).collect(),
        }
    }
}

pub(super) fn execute_probe(path: Option<&str>, issue: Option<u64>, session: Option<&str>) -> i32 {
    match build_probe(path, issue, session) {
        Ok(payload) => print_json(&payload),
        Err(error) => structured_error(
            serde_json::json!({
                "kind": error.kind,
                "message": error.message,
            }),
            1,
        ),
    }
}

pub(crate) fn build_probe(
    path: Option<&str>,
    issue: Option<u64>,
    session: Option<&str>,
) -> Result<ProbeJson, WorktreeError> {
    let runner = ProcessWorktreeRunner::new();
    if let Some(issue) = issue {
        let storage = open_storage().map_err(|message| WorktreeError::new("config", message))?;
        ensure_schema(&storage).map_err(|message| WorktreeError::new("config", message))?;
        let checkout = current_dir();
        let identity = repo_identity(&runner, &checkout)?;
        return Ok(
            match find_active_lease_for_probe(&storage, &identity, issue, session)? {
                Some(row) => {
                    let target = PathBuf::from(&row.worktree_path);
                    let facts = probe_path(&runner, &target);
                    ProbeJson::resolved_at(row.worktree_path.clone(), facts, Some(row))
                }
                None => ProbeJson::unresolved(),
            },
        );
    }
    let target = match path {
        Some(raw) => PathBuf::from(raw),
        None => current_dir(),
    };
    let facts = probe_path(&runner, &target);
    Ok(ProbeJson::resolved_at(
        target.to_string_lossy().to_string(),
        facts,
        None,
    ))
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
