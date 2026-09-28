//! Durable many-to-many branch/issue storage.
//!
//! One row per `(repo_key, branch, provider, project, external_id)`.
//! Rows are never deleted: `detach` flips `status` to `detached` and
//! stamps `detached_at`, so close history and manual-unlink history stay
//! queryable. The table is created lazily via `ensure_schema`, mirroring
//! the worktree-lease pattern, so pre-existing databases gain it on open
//! with no destructive migration.

//! P2 foundation API; production CLI wiring lands in P3.
#![allow(dead_code)]

use rusqlite::{OptionalExtension, params};

use super::issue_key::IssueKey;

pub const STATUS_LINKED: &str = "linked";
pub const STATUS_DETACHED: &str = "detached";

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS branch_issue_links (
    repo_key TEXT NOT NULL,
    branch TEXT NOT NULL,
    provider TEXT NOT NULL,
    project TEXT NOT NULL,
    external_id TEXT NOT NULL,
    issue_number INTEGER NOT NULL CHECK (issue_number > 0),
    status TEXT NOT NULL CHECK (status IN ('linked', 'detached')),
    source TEXT NOT NULL,
    created_at INTEGER NOT NULL CHECK (created_at > 0),
    updated_at INTEGER NOT NULL CHECK (updated_at > 0),
    detached_at INTEGER CHECK (detached_at IS NULL OR detached_at > 0),
    detached_reason TEXT,
    PRIMARY KEY (repo_key, branch, provider, project, external_id)
);

CREATE INDEX IF NOT EXISTS branch_issue_links_branch_idx
    ON branch_issue_links (repo_key, branch, status);

CREATE INDEX IF NOT EXISTS branch_issue_links_issue_idx
    ON branch_issue_links (provider, project, external_id, status);

CREATE INDEX IF NOT EXISTS branch_issue_links_repo_issue_idx
    ON branch_issue_links (repo_key, provider, project, external_id);
";

pub fn ensure_schema(connection: &rusqlite::Connection) -> Result<(), String> {
    connection
        .execute_batch(SCHEMA)
        .map_err(|error| format!("could not initialise branch link table: {error}"))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LinkParams<'a> {
    pub repo_key: &'a str,
    pub branch: &'a str,
    pub issue: &'a IssueKey,
    pub issue_number: u64,
    pub source: &'a str,
    pub now: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    Created,
    AlreadyLinked,
    Relinked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachOutcome {
    Detached,
    AlreadyDetached,
    NotFound,
}

fn validate_link(params: &LinkParams<'_>) -> Result<(), String> {
    for (value, field) in [
        (params.repo_key, "repo_key"),
        (params.branch, "branch"),
        (params.source, "source"),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{field} must be non-empty"));
        }
        if value.chars().any(|c| c.is_control()) {
            return Err(format!("{field} must not contain control characters"));
        }
    }
    if params.issue_number == 0 {
        return Err("issue_number must be greater than zero".to_owned());
    }
    if params.now <= 0 {
        return Err("now must be greater than zero".to_owned());
    }
    Ok(())
}

/// Link `branch` to `issue`, creating or re-linking the row.
/// Idempotent: an already-linked row is a no-op.
pub fn link(
    connection: &rusqlite::Connection,
    params: &LinkParams<'_>,
) -> Result<LinkOutcome, String> {
    validate_link(params)?;
    let existing: Option<String> = connection
        .query_row(
            "SELECT status FROM branch_issue_links \
             WHERE repo_key = ?1 AND branch = ?2 AND provider = ?3 AND project = ?4 AND external_id = ?5",
            params![
                params.repo_key,
                params.branch,
                params.issue.provider,
                params.issue.project,
                params.issue.external_id
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("could not read branch link: {error}"))?;
    match existing.as_deref() {
        Some(STATUS_LINKED) => Ok(LinkOutcome::AlreadyLinked),
        Some(STATUS_DETACHED) => {
            connection
                .execute(
                    "UPDATE branch_issue_links SET status = 'linked', issue_number = ?6, \
                     source = ?7, updated_at = ?8, detached_at = NULL, detached_reason = NULL \
                     WHERE repo_key = ?1 AND branch = ?2 AND provider = ?3 AND project = ?4 AND external_id = ?5",
                    params![
                        params.repo_key,
                        params.branch,
                        params.issue.provider,
                        params.issue.project,
                        params.issue.external_id,
                        params.issue_number as i64,
                        params.source,
                        params.now
                    ],
                )
                .map_err(|error| format!("could not relink branch: {error}"))?;
            Ok(LinkOutcome::Relinked)
        }
        Some(other) => Err(format!("unknown branch link status '{other}'")),
        None => {
            connection
                .execute(
                    "INSERT INTO branch_issue_links \
                     (repo_key, branch, provider, project, external_id, issue_number, status, source, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'linked', ?7, ?8, ?8)",
                    params![
                        params.repo_key,
                        params.branch,
                        params.issue.provider,
                        params.issue.project,
                        params.issue.external_id,
                        params.issue_number as i64,
                        params.source,
                        params.now
                    ],
                )
                .map_err(|error| format!("could not link branch: {error}"))?;
            Ok(LinkOutcome::Created)
        }
    }
}

/// Explicit manual unlink. Flips a linked row to `detached`, retaining
/// it as history. Never deletes rows.
pub fn detach(
    connection: &rusqlite::Connection,
    repo_key: &str,
    branch: &str,
    issue: &IssueKey,
    reason: &str,
    now: i64,
) -> Result<DetachOutcome, String> {
    if now <= 0 {
        return Err("now must be greater than zero".to_owned());
    }
    let existing: Option<String> = connection
        .query_row(
            "SELECT status FROM branch_issue_links \
             WHERE repo_key = ?1 AND branch = ?2 AND provider = ?3 AND project = ?4 AND external_id = ?5",
            params![repo_key, branch, issue.provider, issue.project, issue.external_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("could not read branch link: {error}"))?;
    match existing.as_deref() {
        None => Ok(DetachOutcome::NotFound),
        Some(STATUS_DETACHED) => Ok(DetachOutcome::AlreadyDetached),
        Some(STATUS_LINKED) => {
            connection
                .execute(
                    "UPDATE branch_issue_links SET status = 'detached', updated_at = ?6, \
                     detached_at = ?6, detached_reason = ?7 \
                     WHERE repo_key = ?1 AND branch = ?2 AND provider = ?3 AND project = ?4 AND external_id = ?5",
                    params![repo_key, branch, issue.provider, issue.project, issue.external_id, now, reason],
                )
                .map_err(|error| format!("could not detach branch: {error}"))?;
            Ok(DetachOutcome::Detached)
        }
        Some(other) => Err(format!("unknown branch link status '{other}'")),
    }
}
