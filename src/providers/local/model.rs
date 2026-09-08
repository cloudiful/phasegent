//! Local provider row mappings (issue 211 P2).
//!
//! Converts `local_issues` / `local_comments` / `local_projects` rows
//! into the shared Redmine-aligned envelopes (`IssueSummary`,
//! `CommentOutput`, `RedmineProject`, `RedmineIssueStatus`,
//! `RedmineVersion`). Status ids and `is_closed` flags are static so
//! the JSON shape matches Redmine without a versions table.

use crate::providers::api::{CommentOutput, ForgejoError, IssueSummary};
use crate::providers::{RedmineIssueStatus, RedmineProject, RedmineVersion};

/// Static local statuses mirroring the canonical workflow. Ids are
/// stable (1-8, alphabetical creation order is NOT used) and
/// `is_closed` is true only for the two terminal statuses.
pub(crate) const LOCAL_STATUSES: &[(u64, &str, bool)] = &[
    (1, "New", false),
    (2, "In Progress", false),
    (3, "In Review", false),
    (4, "Changes Requested", false),
    (5, "Blocked", false),
    (6, "Resolved", false),
    (7, "Closed", true),
    (8, "Cancelled", true),
];

pub(crate) struct LocalIssueRow {
    pub(crate) id: i64,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) status: String,
    // Reserved for the future index view (P5/cross_backend); the current
    // Redmine envelope only consumes id/title/body/status. Kept loaded so
    // SELECTs stay stable while the index surface is designed.
    #[allow(dead_code)]
    pub(crate) project: String,
    #[allow(dead_code)]
    pub(crate) tracker: String,
    #[allow(dead_code)]
    pub(crate) author_role: String,
    #[allow(dead_code)]
    pub(crate) created_at: i64,
    #[allow(dead_code)]
    pub(crate) updated_at: i64,
    #[allow(dead_code)]
    pub(crate) closed_at: Option<i64>,
}

impl LocalIssueRow {
    pub(crate) fn into_summary(self) -> IssueSummary {
        let number = self.id as u64;
        IssueSummary {
            id: number,
            number,
            title: self.title,
            body: self.body,
            state: state_for_status(&self.status).to_owned(),
            html_url: Some(local_issue_url(number)),
        }
    }
}

pub(crate) struct LocalCommentRow {
    pub(crate) id: i64,
    pub(crate) issue_id: i64,
    pub(crate) marker: String,
    pub(crate) body: String,
}

impl LocalCommentRow {
    pub(crate) fn to_create_output(&self) -> CommentOutput {
        CommentOutput {
            id: self.id as u64,
            html_url: Some(local_comment_url(self.issue_id as u64, self.id as u64)),
            marker: Some(self.marker.clone()),
            body: None,
        }
    }

    pub(crate) fn to_get_output(&self) -> CommentOutput {
        CommentOutput {
            id: self.id as u64,
            html_url: Some(local_comment_url(self.issue_id as u64, self.id as u64)),
            marker: Some(self.marker.clone()),
            body: Some(self.body.clone()),
        }
    }

    pub(crate) fn to_find_output(&self) -> CommentOutput {
        CommentOutput {
            id: self.id as u64,
            html_url: Some(local_comment_url(self.issue_id as u64, self.id as u64)),
            marker: Some(self.marker.clone()),
            body: None,
        }
    }
}

pub(crate) struct LocalProjectRow {
    pub(crate) id: i64,
    pub(crate) project: String,
    pub(crate) description: String,
}

impl LocalProjectRow {
    pub(crate) fn into_project(self) -> RedmineProject {
        RedmineProject {
            id: self.id as u64,
            name: self.project.clone(),
            identifier: self.project,
            description: self.description,
            status: None,
            is_public: None,
            inherit_members: None,
            created_on: None,
            updated_on: None,
        }
    }
}

pub(crate) fn local_statuses() -> Vec<RedmineIssueStatus> {
    LOCAL_STATUSES
        .iter()
        .map(|(id, name, is_closed)| RedmineIssueStatus {
            id: *id,
            name: (*name).to_owned(),
            is_closed: *is_closed,
        })
        .collect()
}

pub(crate) fn is_closed_status(status: &str) -> bool {
    matches!(status, "Closed" | "Cancelled")
}

pub(crate) fn state_for_status(status: &str) -> &'static str {
    if is_closed_status(status) {
        "closed"
    } else {
        "open"
    }
}

pub(crate) fn local_issue_url(id: u64) -> String {
    format!("local:///issues/{id}")
}

pub(crate) fn local_comment_url(issue: u64, comment: u64) -> String {
    format!("local:///issues/{issue}#note-{comment}")
}

pub(crate) fn now_epoch_seconds() -> i64 {
    crate::time_tracking::util::now_epoch_seconds()
}

/// Load one named query from `local_sql/queries.sql`. The file is the
/// single source for static SQL; Rust only carries the `-- name:`
/// selector so `cargo check` needs no live database.
pub(crate) fn local_sql(name: &str) -> &'static str {
    static QUERIES: &str = include_str!("../../infra/local_sql/queries.sql");
    let header = format!("-- name: {name}");
    let start = QUERIES.find(header.as_str()).unwrap_or_else(|| {
        panic!("local query '{name}' is missing from queries.sql");
    });
    let after = &QUERIES[start + header.len()..];
    let end = after.find("-- name:").map(|i| start + header.len() + i);
    let sql = match end {
        Some(end) => &QUERIES[start + header.len()..end],
        None => after,
    };
    sql.trim()
}

pub(crate) fn is_unique_violation(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(failure, message) => {
            failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                || message.as_deref().unwrap_or_default().contains("UNIQUE")
        }
        _ => error.to_string().contains("UNIQUE"),
    }
}

pub(crate) fn db_error(operation: &str, error: rusqlite::Error) -> ForgejoError {
    ForgejoError::request(operation, bounded(&error.to_string()))
}

pub(crate) fn empty_versions() -> Vec<RedmineVersion> {
    Vec::new()
}

fn bounded(message: &str) -> String {
    message.chars().take(400).collect::<String>()
}
