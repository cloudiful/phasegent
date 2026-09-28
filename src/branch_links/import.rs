//! Per-clone idempotent import of legacy Git-config bindings.
//!
//! Legacy state is one issue id per branch in the checkout's own
//! `.git/config` (`branch.<name>.redmine-issue-id`). Import reads those
//! keys and inserts durable link rows; it never writes or deletes the
//! source Git keys. Re-running the import is a no-op, and rows the
//! operator explicitly detached stay detached.

//! P2 foundation API; production CLI wiring lands in P3.
#![allow(dead_code)]

use crate::branch_context::{CONFIG_KEY_SUFFIX, GitRunner};

use super::issue_key::IssueKey;
use super::store::{LinkOutcome, LinkParams, STATUS_DETACHED};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyBinding {
    pub branch: String,
    pub issue_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    pub imported: usize,
    pub already_present: usize,
    pub skipped_invalid: usize,
}

/// List every legacy `branch.<name>.redmine-issue-id` binding in the
/// current checkout. Malformed values are skipped, not fatal.
pub fn list_legacy_bindings(runner: &dyn GitRunner) -> Result<Vec<LegacyBinding>, String> {
    let output = runner
        .run(&["config", "--local", "--get-regexp", &get_regexp()])
        .map_err(|error| format!("could not list legacy bindings: {}", error.message))?;
    if output.status != 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for line in output.stdout.lines() {
        if let Some(binding) = parse_get_regexp_line(line) {
            out.push(binding);
        }
    }
    out.sort_by(|a, b| (&a.branch, a.issue_id).cmp(&(&b.branch, b.issue_id)));
    out.dedup_by(|a, b| a.branch == b.branch && a.issue_id == b.issue_id);
    Ok(out)
}

fn get_regexp() -> String {
    format!(r"^branch\..*\.{CONFIG_KEY_SUFFIX}$")
}

fn parse_get_regexp_line(line: &str) -> Option<LegacyBinding> {
    let (key, raw) = line.split_once(char::is_whitespace)?;
    let key = key.trim();
    let raw = raw.trim();
    let branch = key
        .strip_prefix("branch.")?
        .strip_suffix(&format!(".{CONFIG_KEY_SUFFIX}"))?;
    if branch.is_empty() || branch.contains(char::is_whitespace) {
        return None;
    }
    let issue_id = raw.parse::<u64>().ok().filter(|id| *id > 0)?;
    Some(LegacyBinding {
        branch: branch.to_owned(),
        issue_id,
    })
}

/// Import `bindings` under `repo_key` with `provider`/`project` scope.
/// Detached rows are left detached so an explicit manual unlink is not
/// resurrected by a later import.
pub fn import_legacy_bindings(
    connection: &rusqlite::Connection,
    repo_key: &str,
    bindings: &[LegacyBinding],
    provider: &str,
    project: &str,
    now: i64,
) -> Result<ImportSummary, String> {
    if now <= 0 {
        return Err("now must be greater than zero".to_owned());
    }
    let mut summary = ImportSummary {
        imported: 0,
        already_present: 0,
        skipped_invalid: 0,
    };
    for binding in bindings {
        if binding.branch.trim().is_empty() || binding.issue_id == 0 {
            summary.skipped_invalid += 1;
            continue;
        }
        let issue = match IssueKey::from_number(provider, project, binding.issue_id) {
            Ok(key) => key,
            Err(_) => {
                summary.skipped_invalid += 1;
                continue;
            }
        };
        if is_detached(connection, repo_key, &binding.branch, &issue)? {
            summary.already_present += 1;
            continue;
        }
        let outcome = super::store::link(
            connection,
            &LinkParams {
                repo_key,
                branch: &binding.branch,
                issue: &issue,
                issue_number: binding.issue_id,
                source: super::identity::LEGACY_SOURCE,
                now,
            },
        )?;
        match outcome {
            LinkOutcome::Created | LinkOutcome::Relinked => summary.imported += 1,
            LinkOutcome::AlreadyLinked => summary.already_present += 1,
        }
    }
    Ok(summary)
}

fn is_detached(
    connection: &rusqlite::Connection,
    repo_key: &str,
    branch: &str,
    issue: &IssueKey,
) -> Result<bool, String> {
    use rusqlite::{OptionalExtension, params};
    let status: Option<String> = connection
        .query_row(
            "SELECT status FROM branch_issue_links \
             WHERE repo_key = ?1 AND branch = ?2 AND provider = ?3 AND project = ?4 AND external_id = ?5",
            params![repo_key, branch, issue.provider, issue.project, issue.external_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("could not read branch link: {error}"))?;
    Ok(status.as_deref() == Some(STATUS_DETACHED))
}
