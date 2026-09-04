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
use self::issue_index_store::{
    doc_from_row, ensure_fts_populated, list_active_keys_for_scope, load_chunks,
};
use crate::infra::issue_index_schema::{PRAGMA_STATEMENTS_INDEX, SCHEMA_INDEX};
use crate::providers::index::{
    ISSUE_INDEX_MAX_LIST_LIMIT, ISSUE_INDEX_SEARCH_MAX_LIMIT, IssueIndexDocument, IssueIndexKey,
    IssueIndexListOptions, IssueIndexStore,
};
use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
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
    fn load_chunks(
        &self,
        key: &IssueIndexKey,
    ) -> Result<Vec<crate::providers::index::IssueIndexChunk>, String> {
        load_chunks(&self.connection, key)
    }
    fn doc_from_row(
        &self,
        source: String,
        project: String,
        external_id: String,
        issue_number: i64,
        title: String,
        body: String,
        state: String,
        url: Option<String>,
        provider_updated_at: Option<i64>,
        indexed_at: i64,
        content_hash: String,
        deleted: i64,
        deleted_at: Option<i64>,
    ) -> Result<IssueIndexDocument, String> {
        doc_from_row(
            &self.connection,
            source,
            project,
            external_id,
            issue_number,
            title,
            body,
            state,
            url,
            provider_updated_at,
            indexed_at,
            content_hash,
            deleted,
            deleted_at,
        )
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
    async fn get(&self, key: &IssueIndexKey) -> Result<Option<IssueIndexDocument>, String> {
        key.validate()?;
        let mut stmt = self
            .connection
            .prepare(
                "SELECT source,project,external_id,issue_number,title,body,state,url,provider_updated_at,indexed_at,content_hash,deleted,deleted_at \
                 FROM issue_documents WHERE source=?1 AND project=?2 AND external_id=?3",
            )
            .map_err(|e| format!("could not prepare document get: {e}"))?;
        let row = stmt
            .query_row(params![key.source, key.project, key.external_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, String>(10)?,
                    r.get::<_, i64>(11)?,
                    r.get::<_, Option<i64>>(12)?,
                ))
            })
            .optional()
            .map_err(|e| format!("could not read document: {e}"))?;
        match row {
            None => Ok(None),
            Some((
                source,
                project,
                external_id,
                issue_number,
                title,
                body,
                state,
                url,
                provider_updated_at,
                indexed_at,
                content_hash,
                deleted,
                deleted_at,
            )) => {
                let doc = self.doc_from_row(
                    source,
                    project,
                    external_id,
                    issue_number,
                    title,
                    body,
                    state,
                    url,
                    provider_updated_at,
                    indexed_at,
                    content_hash,
                    deleted,
                    deleted_at,
                )?;
                Ok(Some(doc))
            }
        }
    }
    async fn list(&self, opts: &IssueIndexListOptions) -> Result<Vec<IssueIndexDocument>, String> {
        if opts.limit == 0 || opts.limit > ISSUE_INDEX_MAX_LIST_LIMIT {
            return Err(format!(
                "list limit must be between 1 and {}",
                ISSUE_INDEX_MAX_LIST_LIMIT
            ));
        }
        let mut stmt = self
            .connection
            .prepare(
                "SELECT source,project,external_id,issue_number,title,body,state,url,provider_updated_at,indexed_at,content_hash,deleted,deleted_at \
                 FROM issue_documents WHERE deleted=0 ORDER BY source ASC, project ASC, external_id ASC LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| format!("could not prepare document list: {e}"))?;
        let rows = stmt
            .query_map(params![opts.limit as i64, opts.offset as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, String>(10)?,
                    r.get::<_, i64>(11)?,
                    r.get::<_, Option<i64>>(12)?,
                ))
            })
            .map_err(|e| format!("could not read document list: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            let (
                source,
                project,
                external_id,
                issue_number,
                title,
                body,
                state,
                url,
                provider_updated_at,
                indexed_at,
                content_hash,
                deleted,
                deleted_at,
            ) = r.map_err(|e| format!("could not decode list row: {e}"))?;
            out.push(self.doc_from_row(
                source,
                project,
                external_id,
                issue_number,
                title,
                body,
                state,
                url,
                provider_updated_at,
                indexed_at,
                content_hash,
                deleted,
                deleted_at,
            )?);
        }
        Ok(out)
    }
    async fn tombstone(&self, key: &IssueIndexKey, indexed_at: i64) -> Result<(), String> {
        key.validate()?;
        if indexed_at <= 0 {
            return Err("indexed_at must be greater than zero".to_owned());
        }
        let placeholder_title = "deleted";
        let placeholder_body = "";
        let placeholder_state = "deleted";
        let placeholder_hash = crate::providers::index::content_hash(
            placeholder_title,
            placeholder_body,
            placeholder_state,
        );
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(|e| format!("could not begin tombstone: {e}"))?;
        tx.execute(
            "INSERT INTO issue_documents \
             (source,project,external_id,issue_number,title,body,state,url,provider_updated_at,indexed_at,content_hash,deleted,deleted_at) \
             VALUES (?1,?2,?3,0,?4,?5,?6,NULL,NULL,?7,?8,1,?7) \
             ON CONFLICT(source,project,external_id) DO UPDATE SET deleted=1, deleted_at=excluded.deleted_at, indexed_at=excluded.indexed_at",
            params![
                key.source,
                key.project,
                key.external_id,
                placeholder_title,
                placeholder_body,
                placeholder_state,
                indexed_at,
                placeholder_hash
            ],
        )
        .map_err(|e| format!("could not tombstone document: {e}"))?;
        tx.execute(
            "DELETE FROM issue_chunks WHERE source=?1 AND project=?2 AND external_id=?3",
            params![key.source, key.project, key.external_id],
        )
        .map_err(|e| format!("could not delete tombstoned chunks: {e}"))?;
        tx.execute(
            "DELETE FROM issue_fts WHERE rowid = (SELECT rowid FROM issue_documents WHERE source=?1 AND project=?2 AND external_id=?3)",
            params![key.source, key.project, key.external_id],
        )
        .map_err(|e| format!("could not delete tombstoned FTS: {e}"))?;
        tx.commit()
            .map_err(|e| format!("could not commit tombstone: {e}"))?;
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

    async fn list_active_keys_for_scope(
        &self,
        source: &str,
        project: &str,
    ) -> Result<Vec<IssueIndexKey>, String> {
        list_active_keys_for_scope(&self.connection, source, project)
    }
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    self::issue_index_store::create_private_dir(path)
}
fn project_dirs_index_path() -> Result<PathBuf, String> {
    self::issue_index_store::project_dirs_index_path()
}
