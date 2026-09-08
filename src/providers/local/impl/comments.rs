//! Local comment lifecycle.

use super::model::{LocalCommentRow, is_unique_violation, local_sql, now_epoch_seconds};
use super::LocalProvider;
use crate::providers::api::{CommentOutput, ForgejoError};

fn row_from_stmt(row: &rusqlite::Row<'_>) -> Result<LocalCommentRow, rusqlite::Error> {
    Ok(LocalCommentRow {
        id: row.get(0)?,
        issue_id: row.get(1)?,
        marker: row.get(5)?,
        body: row.get(6)?,
    })
}

impl LocalProvider {
    pub fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, ForgejoError> {
        if issue == 0 {
            return Err(ForgejoError::config("issue number must be greater than zero"));
        }
        if marker.is_empty() {
            return Err(ForgejoError::config("marker cannot be empty"));
        }
        // Friendly existence check before the UNIQUE insert so a missing
        // issue surfaces as not-found instead of a foreign-key error.
        self.get_issue(issue).map_err(|error| {
            let message = error.to_string();
            if message.contains("was not found") {
                ForgejoError::not_found("comment create", &format!("issue {issue} was not found"))
            } else {
                error
            }
        })?;
        let now = now_epoch_seconds();
        let body_owned = body.to_owned();
        let marker_owned = marker.to_owned();
        let inner = self
            .with_conn("comment create", |conn| {
                match conn.execute(
                    local_sql("insert_comment"),
                    rusqlite::params![
                        issue as i64,
                        "executor",
                        "",
                        1_i64,
                        marker_owned,
                        body_owned,
                        now,
                    ],
                ) {
                    Ok(_) => Ok(Ok(conn.last_insert_rowid())),
                    Err(error) => match friendly_marker_error(marker, &error) {
                        Some(friendly) => Ok(Err(friendly)),
                        None => Err(error),
                    },
                }
            })
            .map_err(|error| {
                if error.to_string().contains("FOREIGN KEY") {
                    ForgejoError::not_found(
                        "comment create",
                        &format!("issue {issue} was not found"),
                    )
                } else {
                    error
                }
            })?;
        let id = inner?;
        Ok(LocalCommentRow {
            id,
            issue_id: issue as i64,
            marker: marker.to_owned(),
            body: body.to_owned(),
        }
        .to_create_output())
    }

    pub fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, ForgejoError> {
        if issue == 0 || comment == 0 {
            return Err(ForgejoError::config("issue and comment ids must be greater than zero"));
        }
        self.with_conn("comment get", |conn| {
            conn.query_row(
                local_sql("get_comment"),
                rusqlite::params![comment as i64, issue as i64],
                row_from_stmt,
            )
        })
        .map(|row| row.to_get_output())
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("QueryReturnedNoRows") || message.contains("no rows") {
                ForgejoError::not_found(
                    "comment get",
                    "comment was not found in the specified issue",
                )
            } else {
                error
            }
        })
    }

    pub fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, ForgejoError> {
        if marker.is_empty() {
            return Err(ForgejoError::config("marker cannot be empty"));
        }
        if issue == 0 {
            return Err(ForgejoError::config("issue number must be greater than zero"));
        }
        let marker_owned = marker.to_owned();
        self.with_conn("comment find-marker", |conn| {
            conn.query_row(
                local_sql("find_comment_by_marker"),
                rusqlite::params![issue as i64, marker_owned],
                row_from_stmt,
            )
        })
        .map(|row| row.to_find_output())
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("QueryReturnedNoRows") || message.contains("no rows") {
                ForgejoError::not_found("comment find-marker", "marker was not found")
            } else {
                error
            }
        })
    }
}

/// Map a raw rusqlite UNIQUE failure to the friendly marker error.
pub(crate) fn friendly_marker_error(marker: &str, error: &rusqlite::Error) -> Option<ForgejoError> {
    if is_unique_violation(error) {
        Some(ForgejoError::request(
            "comment create",
            format!("marker '{marker}' already exists; markers must be unique"),
        ))
    } else {
        None
    }
}
