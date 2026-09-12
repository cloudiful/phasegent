//! Local issue CRUD + search.

use super::LocalProvider;
use super::model::{LocalIssueRow, local_sql, now_epoch_seconds};
use super::status_impl::{allowed_next_for, is_transition_allowed};
use crate::providers::api::{
    ForgejoError, IssueSearchItem, IssueSearchOptions, IssueSearchResult, IssueSummary,
    IssueSummaryPage,
};

fn row_from_stmt(row: &rusqlite::Row<'_>) -> Result<LocalIssueRow, rusqlite::Error> {
    Ok(LocalIssueRow {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        status: row.get(3)?,
        project: row.get(4)?,
        tracker: row.get(5)?,
        author_role: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        closed_at: row.get(9)?,
    })
}

fn fetch_one(conn: &rusqlite::Connection, id: u64) -> Result<LocalIssueRow, rusqlite::Error> {
    conn.query_row(
        local_sql("get_issue_by_id"),
        rusqlite::params![id as i64],
        row_from_stmt,
    )
}

impl LocalProvider {
    pub fn get_issue(&self, number: u64) -> Result<IssueSummary, ForgejoError> {
        if number == 0 {
            return Err(ForgejoError::config(
                "issue number must be greater than zero",
            ));
        }
        self.with_conn("issue get", |conn| fetch_one(conn, number))
            .map_err(|error| match_not_found(error, number, "issue get"))
            .map(|row| row.into_summary())
    }

    pub fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, ForgejoError> {
        options.validate()?;
        let query = options.effective_query().unwrap_or_default().to_owned();
        let offset = (options.page.saturating_sub(1)).saturating_mul(options.limit);
        let (total, rows) = self.with_conn("issue search", |conn| {
            let total: i64 = conn.query_row(
                local_sql("count_issues"),
                rusqlite::params![options.state, query,],
                |row| row.get(0),
            )?;
            let mut stmt = conn.prepare(local_sql("search_issues"))?;
            let items = stmt
                .query_map(
                    rusqlite::params![options.state, query, options.limit as i64, offset as i64,],
                    row_from_stmt,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok((total, items))
        })?;
        let total_count = usize::try_from(total).ok();
        let count = rows.len();
        let has_more = total_count.map_or(count == options.limit, |total| offset + count < total);
        let items = rows
            .into_iter()
            .map(|row| {
                let summary = row.into_summary();
                IssueSearchItem::from_summary(summary, options.include_body)
            })
            .collect();
        Ok(IssueSearchResult {
            items,
            page: options.page,
            limit: options.limit,
            total_count,
            has_more: has_more && count > 0,
        })
    }

    pub fn search_issue_page(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSummaryPage, ForgejoError> {
        options.validate()?;
        let query = options.effective_query().unwrap_or_default().to_owned();
        let offset = (options.page.saturating_sub(1)).saturating_mul(options.limit);
        let (total, rows) = self.with_conn("issue search", |conn| {
            let total: i64 = conn.query_row(
                local_sql("count_issues"),
                rusqlite::params![options.state, query,],
                |row| row.get(0),
            )?;
            let mut stmt = conn.prepare(local_sql("search_issues"))?;
            let items = stmt
                .query_map(
                    rusqlite::params![options.state, query, options.limit as i64, offset as i64,],
                    row_from_stmt,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok((total, items))
        })?;
        let total_count = usize::try_from(total).ok();
        let count = rows.len();
        let has_more = total_count.map_or(count == options.limit, |total| offset + count < total);
        Ok(IssueSummaryPage {
            items: rows.into_iter().map(|row| row.into_summary()).collect(),
            page: options.page,
            limit: options.limit,
            total_count,
            has_more: has_more && count > 0,
        })
    }

    pub fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, ForgejoError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(ForgejoError::config("issue title cannot be empty"));
        }
        if title.len() > 1024 {
            return Err(ForgejoError::config(
                "issue title must be at most 1024 bytes",
            ));
        }
        let now = now_epoch_seconds();
        let title_owned = title.to_owned();
        let body_owned = body.to_owned();
        let id = self.with_conn("issue create", |conn| {
            conn.execute(
                local_sql("insert_issue"),
                rusqlite::params![
                    title_owned,
                    body_owned,
                    "New",
                    "default",
                    "Task",
                    "executor",
                    now,
                    now,
                ],
            )?;
            Ok(conn.last_insert_rowid())
        })?;
        self.get_issue(id as u64)
    }

    pub fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, ForgejoError> {
        if number == 0 {
            return Err(ForgejoError::config(
                "issue number must be greater than zero",
            ));
        }
        let now = now_epoch_seconds();
        let body_owned = body.to_owned();
        let changed = self.with_conn("issue update", |conn| {
            let changed = conn.execute(
                local_sql("update_issue_body"),
                rusqlite::params![body_owned, now, number as i64],
            )?;
            if changed == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            Ok(changed)
        });
        match changed {
            Err(error) if is_no_rows(&error) => Err(ForgejoError::not_found(
                "issue update",
                &format!("issue {number} was not found"),
            )),
            Err(error) => Err(error),
            Ok(_) => self.get_issue(number),
        }
    }

    pub fn close_issue(&self, number: u64) -> Result<IssueSummary, ForgejoError> {
        if number == 0 {
            return Err(ForgejoError::config(
                "issue number must be greater than zero",
            ));
        }
        let current: LocalIssueRow = self
            .with_conn("issue close", |conn| fetch_one(conn, number))
            .map_err(|error| match_not_found(error, number, "issue close"))?;
        if current.status == "Closed" {
            return Ok(current.into_summary());
        }
        if !is_transition_allowed(&current.status, "Closed") {
            let allowed = allowed_next_for(&current.status);
            let hint = if allowed.is_empty() {
                "<none: terminal status>".to_owned()
            } else {
                allowed.join(", ")
            };
            return Err(ForgejoError::request(
                "issue close",
                format!(
                    "transition rejected before any write: current status '{}' -> 'Closed' is not allowed by policy phasegent/canonical-phase-workflow@v1; allowed_next=[{hint}]",
                    current.status,
                ),
            ));
        }
        let now = now_epoch_seconds();
        self.with_conn("issue close", |conn| {
            let changed = conn.execute(
                local_sql("close_issue"),
                rusqlite::params![now, now, number as i64],
            )?;
            if changed == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            Ok(())
        })
        .map_err(|error| match_not_found(error, number, "issue close"))?;
        self.get_issue(number)
    }
}

fn is_no_rows(error: &ForgejoError) -> bool {
    error.to_string().contains("QueryReturnedNoRows") || error.to_string().contains("no rows")
}

fn match_not_found(error: ForgejoError, number: u64, operation: &str) -> ForgejoError {
    let message = error.to_string();
    if message.contains("QueryReturnedNoRows") || message.contains("no rows") {
        ForgejoError::not_found(operation, &format!("issue {number} was not found"))
    } else {
        error
    }
}
