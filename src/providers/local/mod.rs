//! Local provider entry.
//!
//! SQLite-backed [`LocalProvider`] implements the shared
//! `IssueProvider` / `RedmineMetadataProvider` / `RepoProvider`
//! surfaces; PostgreSQL stays a reserved stub (`PgLocalProvider`)
//! that fails with a structured not-supported error. Planning fields
//! (`--parent-issue`, `--fixed-version`, dates, estimates) are
//! accepted by the CLI but intentionally ignored and never persisted.

pub mod model;

#[path = "impl/comments.rs"]
mod comments;
#[path = "impl/issues.rs"]
mod issues;
#[path = "impl/projects.rs"]
mod projects_impl;
#[path = "impl/status.rs"]
mod status_impl;

#[cfg(test)]
mod contract_tests;

use crate::policy::Capability;
use crate::providers::api::ForgejoError;
use std::path::Path;
use std::sync::Mutex;

/// SQLite-backed local provider. Holds one connection behind a mutex
/// so the shared `&self` trait surface can run blocking queries.
pub struct LocalProvider {
    conn: Mutex<rusqlite::Connection>,
}

impl std::fmt::Debug for LocalProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalProvider")
            .finish_non_exhaustive()
    }
}

impl LocalProvider {
    pub fn open() -> Result<Self, ForgejoError> {
        let store = crate::infra::local_store::open_local().map_err(ForgejoError::config)?;
        Ok(Self {
            conn: Mutex::new(store.connection),
        })
    }

    #[allow(dead_code)]
    pub fn open_at(path: &Path) -> Result<Self, ForgejoError> {
        let store = crate::infra::local_store::open_local_at(path).map_err(ForgejoError::config)?;
        Ok(Self {
            conn: Mutex::new(store.connection),
        })
    }

    pub(crate) fn with_conn<R>(
        &self,
        operation: &'static str,
        f: impl FnOnce(&rusqlite::Connection) -> Result<R, rusqlite::Error>,
    ) -> Result<R, ForgejoError> {
        let guard = self.conn.lock().map_err(|_| {
            ForgejoError::request(operation, "local database lock was poisoned".to_owned())
        })?;
        f(&guard).map_err(|error| model::db_error(operation, error))
    }

    pub(crate) fn capabilities(&self) -> crate::providers::ProviderCapabilities {
        crate::providers::ProviderCapabilities {
            issue_lifecycle: true,
            comments: true,
            repository_creation: false,
        }
    }

    pub(crate) fn supports(&self, capability: Capability) -> bool {
        match capability {
            Capability::IssueRead
            | Capability::IssueSearch
            | Capability::IssueCreate
            | Capability::IssueUpdateBody
            | Capability::IssueClose
            | Capability::CommentCreate
            | Capability::CommentRead
            | Capability::CommentFindMarker
            | Capability::ProjectRead
            | Capability::ProjectCreate
            | Capability::IssueStatusRead
            | Capability::VersionRead => true,
            Capability::IssueAttachmentUpload
            | Capability::RepoCreate
            | Capability::RelationRead
            | Capability::RelationCreate
            | Capability::RelationDelete => false,
        }
    }
}

/// Reserved PostgreSQL local provider.
///
/// SQLite is authoritative; this struct only reserves the interface
/// so the PostgreSQL backend can add CRUD without renaming. `open`
/// intentionally fails with a structured not-supported error.
#[derive(Debug)]
#[allow(dead_code)]
pub struct PgLocalProvider {
    _private: (),
}

#[allow(dead_code)]
impl PgLocalProvider {
    pub fn open(_url: &str) -> Result<Self, ForgejoError> {
        // PG reserved by design: implement PostgresLocalStore-backed
        // CRUD mirroring Sqlite LocalProvider once async dispatch is wired.
        Err(ForgejoError::not_supported(
            "local",
            "postgres backend (reserved)",
        ))
    }
}
