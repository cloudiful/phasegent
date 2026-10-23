use super::LocalProvider;
use super::model::{LocalProjectRow, empty_versions, local_sql, now_epoch_seconds};
use crate::providers::api::PhasegentError;
use crate::providers::{RedmineProject, RedmineVersion};

fn row_from_stmt(row: &rusqlite::Row<'_>) -> Result<LocalProjectRow, rusqlite::Error> {
    Ok(LocalProjectRow {
        id: row.get(0)?,
        project: row.get(1)?,
        description: row.get(2)?,
    })
}

impl LocalProvider {
    pub(crate) fn list_projects_page(
        &self,
        page: crate::pagination::PageRequest,
    ) -> Result<crate::pagination::Paged<RedmineProject>, PhasegentError> {
        self.with_conn("project list", |conn| {
            let transaction = conn.unchecked_transaction()?;
            let total = transaction.query_row(local_sql("count_projects"), [], |row| row.get(0))?;
            let items = {
                let mut statement = transaction.prepare(local_sql("page_projects"))?;
                statement
                    .query_map(
                        rusqlite::params![page.limit() as i64, page.offset() as i64],
                        row_from_stmt,
                    )?
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(|row| row.into_project())
                    .collect()
            };
            transaction.commit()?;
            Ok(crate::pagination::Paged::known(items, page, total))
        })
    }
    pub(crate) fn list_versions_page(
        &self,
        page: crate::pagination::PageRequest,
    ) -> Result<crate::pagination::Paged<RedmineVersion>, PhasegentError> {
        Ok(crate::pagination::Paged::known(empty_versions(), page, 0))
    }

    pub fn list_projects(&self) -> Result<Vec<RedmineProject>, PhasegentError> {
        let rows = self.with_conn("project list", |conn| {
            let mut stmt = conn.prepare(local_sql("list_projects"))?;
            let rows = stmt
                .query_map([], row_from_stmt)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        Ok(rows.into_iter().map(|row| row.into_project()).collect())
    }

    pub fn create_project(
        &self,
        name: &str,
        identifier: &str,
        description: Option<&str>,
    ) -> Result<RedmineProject, PhasegentError> {
        if name.trim().is_empty() {
            return Err(PhasegentError::config("project name cannot be empty"));
        }
        if identifier.trim().is_empty() {
            return Err(PhasegentError::config("project identifier cannot be empty"));
        }
        if identifier.len() > 200 {
            return Err(PhasegentError::config(
                "project identifier must be at most 200 bytes",
            ));
        }
        // Local stores a single `project` key; the identifier is
        // authoritative and the display name mirrors it so list/create
        // agree. The `name` argument is validated but not stored.
        let key = identifier.trim().to_owned();
        let description_owned = description.unwrap_or_default().to_owned();
        let now = now_epoch_seconds();
        let created = self.with_conn("project create", |conn| {
            conn.execute(
                local_sql("insert_project"),
                rusqlite::params![key, description_owned, now],
            )?;
            Ok(conn.last_insert_rowid())
        });
        match created {
            Ok(rowid) => Ok(RedmineProject {
                id: rowid as u64,
                name: key.clone(),
                identifier: key,
                description: description.unwrap_or_default().to_owned(),
                status: None,
                is_public: None,
                inherit_members: None,
                created_on: None,
                updated_on: None,
            }),
            Err(error) => {
                let message = error.to_string();
                if message.contains("UNIQUE") || message.contains("already exists") {
                    return Err(PhasegentError::request(
                        "project create",
                        format!("project '{key}' already exists"),
                    ));
                }
                Err(error)
            }
        }
    }

    pub fn list_versions(&self) -> Result<Vec<RedmineVersion>, PhasegentError> {
        // No versions table; return an empty catalogue so the envelope
        // stays Redmine-compatible without inventing rows.
        Ok(empty_versions())
    }
}
