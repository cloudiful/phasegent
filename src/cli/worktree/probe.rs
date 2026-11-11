//! Executor for the read-only `worktree probe` subcommand (issue 595).
//!
//! The command answers one question without side effects: given
//! `--path PATH`, or `--issue N [--session S]`, or neither (the current
//! checkout), what is on disk? It never calls a provider, never writes the
//! registry, never syncs, and never repairs anything. A Git or filesystem
//! failure is reported inside the bounded JSON envelope rather than as a
//! hard error; only an unusable repo identity is a structured error.
//!
//! The role gate lives in the parent module's dispatcher, not here.

use std::path::PathBuf;

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::worktree::probe::probe_path;
use crate::worktree::registry::record_for_issue_session;
use crate::worktree::{
    DirectoryRecord, ProbeError, ProbeFacts, ProcessWorktreeRunner, WorktreeError, repo_identity,
};

use super::query::DirectoryJson;

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
    pub directory: Option<DirectoryJson>,
    pub errors: Vec<ProbeErrorJson>,
}

impl ProbeJson {
    /// The stable empty result for an `--issue` selector that matched no
    /// registered directory: no path is guessed.
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
            directory: None,
            errors: Vec::new(),
        }
    }

    /// Assemble the envelope for a resolved path plus its read-only
    /// facts and (optionally) the directory the selector matched.
    fn resolved_at(path: String, facts: ProbeFacts, directory: Option<DirectoryRecord>) -> Self {
        Self {
            resolved: true,
            path: Some(path),
            exists: facts.exists,
            is_git_worktree: facts.is_git_worktree,
            clean: facts.clean,
            branch: facts.branch,
            head: facts.head,
            is_main_checkout: facts.is_main_checkout,
            directory: directory.map(DirectoryJson::from),
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
        let Some(storage) = crate::infra::storage::Storage::open_existing_read_only()
            .map_err(|message| WorktreeError::new("config", message))?
        else {
            return Ok(ProbeJson::unresolved());
        };
        let checkout = current_dir();
        let identity = repo_identity(&runner, &checkout)?;
        let record = match session {
            Some(session) => record_for_issue_session(&storage, &identity, issue, session)?,
            None => crate::worktree::registry::records_for_issue(&storage, Some(&identity), issue)?
                .into_iter()
                .last(),
        };
        return Ok(match record {
            Some(record) => {
                let target = PathBuf::from(&record.worktree_path);
                let facts = probe_path(&runner, &target);
                let path = record.worktree_path.clone();
                ProbeJson::resolved_at(path, facts, Some(record))
            }
            None => ProbeJson::unresolved(),
        });
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
