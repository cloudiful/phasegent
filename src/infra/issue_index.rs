//! Independent SQLite backend for the issue index.
//!
//! The index file `phasegent-index.sqlite3` lives beside the main config
//! database and never touches credentials. `PHASEGENT_INDEX_DB_PATH`
//! overrides the platform directory for tests and operators.

#![allow(dead_code)]

#[path = "issue_index_search.rs"]
mod issue_index_search;
#[path = "issue_index_store.rs"]
mod issue_index_store;

use self::issue_index_search::{lexical_search_inner, normalize_query};
use self::issue_index_store::ensure_fts_populated;
use crate::infra::issue_index_schema::{PRAGMA_STATEMENTS_INDEX, SCHEMA_INDEX};
use crate::providers::index::{ISSUE_INDEX_SEARCH_MAX_LIMIT, IssueIndexDocument, IssueIndexStore};
use async_trait::async_trait;
use rusqlite::{Connection, params};
use std::fs;
use std::path::{Path, PathBuf};

pub struct SqliteIssueIndex {
    connection: Connection,
    path: PathBuf,
}

impl SqliteIssueIndex {
    pub fn open() -> Result<Self, String> {
        if let Some(p) = std::env::var_os("PHASEGENT_INDEX_DB_PATH") {
            let path = PathBuf::from(p);
            if !path.is_absolute() {
                return Err("PHASEGENT_INDEX_DB_PATH must be an absolute path".to_owned());
            }
            return Self::open_at(&path);
        }
        let path = project_dirs_index_path()?;
        Self::open_at(&path)
    }
    pub fn open_at(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let connection = Connection::open(path)
            .map_err(|e| format!("could not open issue index database: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(path)
                .map_err(|e| format!("could not stat issue index database: {e}"))?;
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(path, perms)
                .map_err(|e| format!("could not secure issue index database: {e}"))?;
        }
        Self::initialise(&connection)?;
        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
    }
    fn initialise(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(PRAGMA_STATEMENTS_INDEX)
            .map_err(|e| format!("could not configure issue index database: {e}"))?;
        conn.execute_batch(SCHEMA_INDEX)
            .map_err(|e| format!("could not initialise issue index schema: {e}"))?;
        ensure_fts_populated(conn)?;
        Ok(())
    }
    pub fn db_path(&self) -> &Path {
        &self.path
    }
}

#[async_trait(?Send)]
impl IssueIndexStore for SqliteIssueIndex {
    async fn upsert(&self, doc: &IssueIndexDocument) -> Result<(), String> {
        doc.validate()?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(|e| format!("could not begin index upsert: {e}"))?;
        tx.execute(
            "INSERT INTO issue_documents \
             (source, project, external_id, issue_number, title, body, state, url, provider_updated_at, indexed_at, content_hash, deleted, deleted_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,NULL) \
             ON CONFLICT(source,project,external_id) DO UPDATE SET \
               issue_number=excluded.issue_number, title=excluded.title, body=excluded.body, \
               state=excluded.state, url=excluded.url, provider_updated_at=excluded.provider_updated_at, \
               indexed_at=excluded.indexed_at, content_hash=excluded.content_hash, deleted=0, deleted_at=NULL",
            params![
                doc.key.source,
                doc.key.project,
                doc.key.external_id,
                doc.issue_number as i64,
                doc.title,
                doc.body,
                doc.state,
                doc.url,
                doc.provider_updated_at,
                doc.indexed_at,
                doc.content_hash
            ],
        )
        .map_err(|e| format!("could not upsert document: {e}"))?;
        tx.execute(
            "DELETE FROM issue_chunks WHERE source=?1 AND project=?2 AND external_id=?3",
            params![doc.key.source, doc.key.project, doc.key.external_id],
        )
        .map_err(|e| format!("could not clear old chunks: {e}"))?;
        for c in &doc.chunks {
            tx.execute(
                "INSERT INTO issue_chunks (source,project,external_id,ordinal,text,byte_start,byte_end,hash) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    doc.key.source,
                    doc.key.project,
                    doc.key.external_id,
                    c.ordinal as i64,
                    c.text,
                    c.byte_start as i64,
                    c.byte_end as i64,
                    c.hash
                ],
            )
            .map_err(|e| format!("could not insert chunk {}: {e}", c.ordinal))?;
        }
        // Keep FTS synchronized atomically within the same transaction.
        tx.execute(
            "DELETE FROM issue_fts WHERE rowid = (SELECT rowid FROM issue_documents WHERE source=?1 AND project=?2 AND external_id=?3)",
            params![doc.key.source, doc.key.project, doc.key.external_id],
        )
        .map_err(|e| format!("could not clear old FTS: {e}"))?;
        tx.execute(
            "INSERT INTO issue_fts(rowid, title, body) VALUES ((SELECT rowid FROM issue_documents WHERE source=?1 AND project=?2 AND external_id=?3), ?4, ?5)",
            params![doc.key.source, doc.key.project, doc.key.external_id, doc.title, doc.body],
        )
        .map_err(|e| format!("could not insert FTS: {e}"))?;
        tx.commit()
            .map_err(|e| format!("could not commit index upsert: {e}"))?;
        Ok(())
    }

    async fn lexical_search(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        include_body: bool,
    ) -> Result<crate::providers::index_store::IssueIndexSearchResult, String> {
        if limit == 0 || limit > ISSUE_INDEX_SEARCH_MAX_LIMIT {
            return Err(format!(
                "search limit must be between 1 and {}",
                ISSUE_INDEX_SEARCH_MAX_LIMIT
            ));
        }
        let escaped = normalize_query(query)?;
        // FTS errors (e.g., malformed after escaping) are surfaced as config
        // errors so the CLI can return a structured failure without crashing.
        lexical_search_inner(&self.connection, &escaped, limit, offset, include_body).map_err(|e| e)
    }

    async fn lexical_search_scoped(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        include_body: bool,
        scope: &crate::providers::index::LexicalScope,
    ) -> Result<crate::providers::index_store::IssueIndexSearchResult, String> {
        if limit == 0 || limit > ISSUE_INDEX_SEARCH_MAX_LIMIT {
            return Err(format!(
                "search limit must be between 1 and {}",
                ISSUE_INDEX_SEARCH_MAX_LIMIT
            ));
        }
        let escaped = normalize_query(query)?;
        self::issue_index_search::lexical_search_scoped_inner(
            &self.connection,
            &escaped,
            limit,
            offset,
            include_body,
            scope,
        )
        .map_err(|e| e)
    }
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    self::issue_index_store::create_private_dir(path)
}
fn project_dirs_index_path() -> Result<PathBuf, String> {
    self::issue_index_store::project_dirs_index_path()
}
