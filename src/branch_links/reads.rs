//! Forward/reverse reads with last-known issue-state projection.
//!
//! State comes from a caller-supplied lookup over the local issue-index
//! snapshot, never a live provider call. A missing snapshot is `unknown`:
//! callers must not claim a branch is free solely from cached state.

//! P2 foundation API; production CLI wiring lands in P3.
#![allow(dead_code)]

use super::issue_key::IssueKey;
use super::store::STATUS_DETACHED;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSnapshot {
    pub state: String,
    pub source: String,
    pub indexed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedIssue {
    pub issue: IssueKey,
    pub issue_number: u64,
    pub status: String,
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub detached_at: Option<i64>,
    pub detached_reason: Option<String>,
    pub state: Option<StateSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedBranch {
    pub branch: String,
    pub issue_number: u64,
    pub status: String,
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub detached_at: Option<i64>,
    pub detached_reason: Option<String>,
    pub state: Option<StateSnapshot>,
}

/// Lookup of the last-known indexed state for one issue key.
pub trait IssueStateLookup {
    fn lookup(&self, issue: &IssueKey) -> Option<StateSnapshot>;
}

/// No state available: every projection is `unknown`.
#[derive(Debug, Clone, Copy)]
pub struct UnknownState;

impl IssueStateLookup for UnknownState {
    fn lookup(&self, _issue: &IssueKey) -> Option<StateSnapshot> {
        None
    }
}

impl<F> IssueStateLookup for F
where
    F: Fn(&IssueKey) -> Option<StateSnapshot>,
{
    fn lookup(&self, issue: &IssueKey) -> Option<StateSnapshot> {
        self(issue)
    }
}

/// Branch -> issues, ordered by `(provider, project, external_id)`.
/// Detached rows are included only when `include_detached` is set.
pub fn issues_for_branch(
    connection: &rusqlite::Connection,
    repo_key: &str,
    branch: &str,
    include_detached: bool,
    states: &dyn IssueStateLookup,
) -> Result<Vec<LinkedIssue>, String> {
    let sql = if include_detached {
        "SELECT provider, project, external_id, issue_number, status, source, \
          created_at, updated_at, detached_at, detached_reason \
         FROM branch_issue_links WHERE repo_key = ?1 AND branch = ?2 \
         ORDER BY provider, project, external_id"
    } else {
        "SELECT provider, project, external_id, issue_number, status, source, \
          created_at, updated_at, detached_at, detached_reason \
         FROM branch_issue_links WHERE repo_key = ?1 AND branch = ?2 AND status = 'linked' \
         ORDER BY provider, project, external_id"
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("could not prepare branch read: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![repo_key, branch], decode_issue_row)
        .map_err(|error| format!("could not read branch links: {error}"))?;
    let mut out = Vec::new();
    for row in rows {
        let mut entry = row.map_err(|error| format!("could not decode branch link: {error}"))?;
        entry.state = states.lookup(&entry.issue);
        if !include_detached && entry.status == STATUS_DETACHED {
            continue;
        }
        out.push(entry);
    }
    Ok(out)
}

/// Issue -> branches within one repository key, ordered by branch.
/// Detached rows are included only when `include_detached` is set.
pub fn branches_for_issue(
    connection: &rusqlite::Connection,
    repo_key: &str,
    issue: &IssueKey,
    include_detached: bool,
    states: &dyn IssueStateLookup,
) -> Result<Vec<LinkedBranch>, String> {
    let sql = if include_detached {
        "SELECT branch, issue_number, status, source, created_at, updated_at, \
          detached_at, detached_reason \
         FROM branch_issue_links \
         WHERE repo_key = ?1 AND provider = ?2 AND project = ?3 AND external_id = ?4 \
         ORDER BY branch"
    } else {
        "SELECT branch, issue_number, status, source, created_at, updated_at, \
          detached_at, detached_reason \
         FROM branch_issue_links \
         WHERE repo_key = ?1 AND provider = ?2 AND project = ?3 AND external_id = ?4 \
           AND status = 'linked' \
         ORDER BY branch"
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("could not prepare issue read: {error}"))?;
    let rows = statement
        .query_map(
            rusqlite::params![repo_key, issue.provider, issue.project, issue.external_id],
            |row| {
                Ok(LinkedBranch {
                    branch: row.get(0)?,
                    issue_number: {
                        let raw: i64 = row.get(1)?;
                        raw.max(0) as u64
                    },
                    status: row.get(2)?,
                    source: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                    detached_at: row.get(6)?,
                    detached_reason: row.get(7)?,
                    state: None,
                })
            },
        )
        .map_err(|error| format!("could not read issue links: {error}"))?;
    let snapshot = states.lookup(issue);
    let mut out = Vec::new();
    for row in rows {
        let mut entry = row.map_err(|error| format!("could not decode issue link: {error}"))?;
        entry.state = snapshot.clone();
        if !include_detached && entry.status == STATUS_DETACHED {
            continue;
        }
        out.push(entry);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedBranch {
    pub branch: String,
    pub issue: IssueKey,
    pub issue_number: u64,
    pub status: String,
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub detached_at: Option<i64>,
    pub detached_reason: Option<String>,
    pub state: Option<StateSnapshot>,
}

/// Numeric reverse lookup across every provider/project scope in one
/// repository, ordered by `(provider, project, external_id, branch)`.
/// Powers `issue branches N`: same-number rows from distinct scopes stay
/// distinct so callers see the ambiguity instead of a guessed scope.
/// Detached rows are included only when `include_detached` is set.
pub fn branches_for_number(
    connection: &rusqlite::Connection,
    repo_key: &str,
    issue_number: u64,
    include_detached: bool,
    states: &dyn IssueStateLookup,
) -> Result<Vec<ScopedBranch>, String> {
    let number = i64::try_from(issue_number).unwrap_or(i64::MAX);
    let sql = if include_detached {
        "SELECT provider, project, external_id, branch, issue_number, status, source, \
          created_at, updated_at, detached_at, detached_reason \
         FROM branch_issue_links WHERE repo_key = ?1 AND issue_number = ?2 \
         ORDER BY provider, project, external_id, branch"
    } else {
        "SELECT provider, project, external_id, branch, issue_number, status, source, \
          created_at, updated_at, detached_at, detached_reason \
         FROM branch_issue_links WHERE repo_key = ?1 AND issue_number = ?2 AND status = 'linked' \
         ORDER BY provider, project, external_id, branch"
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("could not prepare number read: {error}"))?;
    let rows = statement
        .query_map(
            rusqlite::params![repo_key, number],
            decode_scoped_branch_row,
        )
        .map_err(|error| format!("could not read number links: {error}"))?;
    let mut out = Vec::new();
    for row in rows {
        let mut entry = row.map_err(|error| format!("could not decode number link: {error}"))?;
        entry.state = states.lookup(&entry.issue);
        if !include_detached && entry.status == STATUS_DETACHED {
            continue;
        }
        out.push(entry);
    }
    Ok(out)
}

fn decode_scoped_branch_row(row: &rusqlite::Row<'_>) -> Result<ScopedBranch, rusqlite::Error> {
    let provider: String = row.get(0)?;
    let project: String = row.get(1)?;
    let external_id: String = row.get(2)?;
    Ok(ScopedBranch {
        issue: IssueKey {
            provider,
            project,
            external_id,
        },
        branch: row.get(3)?,
        issue_number: {
            let raw: i64 = row.get(4)?;
            raw.max(0) as u64
        },
        status: row.get(5)?,
        source: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        detached_at: row.get(9)?,
        detached_reason: row.get(10)?,
        state: None,
    })
}

fn decode_issue_row(row: &rusqlite::Row<'_>) -> Result<LinkedIssue, rusqlite::Error> {
    let provider: String = row.get(0)?;
    let project: String = row.get(1)?;
    let external_id: String = row.get(2)?;
    Ok(LinkedIssue {
        issue: IssueKey {
            provider,
            project,
            external_id,
        },
        issue_number: {
            let raw: i64 = row.get(3)?;
            raw.max(0) as u64
        },
        status: row.get(4)?,
        source: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        detached_at: row.get(8)?,
        detached_reason: row.get(9)?,
        state: None,
    })
}
