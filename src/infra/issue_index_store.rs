//! Scoped active-list helpers for deterministic full-sync tombstones
//! and FTS backfill. Extracted to keep `issue_index.rs` below size thresholds.

use rusqlite::{Connection, params};
use std::fs;
use std::path::{Path, PathBuf};

use crate::infra::issue_index_schema::DB_FILENAME_INDEX;
use crate::providers::index::{IssueIndexChunk, IssueIndexDocument, IssueIndexKey};
use directories::ProjectDirs;

/// List active (non-deleted) keys for a given provider/project scope.
/// Ordered deterministically by source/project/external_id.
pub fn list_active_keys_for_scope(
    conn: &Connection,
    source: &str,
    project: &str,
) -> Result<Vec<IssueIndexKey>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT source, project, external_id FROM issue_documents \
             WHERE source=?1 AND project=?2 AND deleted=0 \
             ORDER BY source ASC, project ASC, external_id ASC",
        )
        .map_err(|e| format!("could not prepare scoped active list: {e}"))?;
    let rows = stmt
        .query_map(params![source, project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("could not query scoped active keys: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        let (source, project, external_id) =
            r.map_err(|e| format!("could not decode scoped key row: {e}"))?;
        let key = IssueIndexKey::new(source, project, external_id)
            .map_err(|e| format!("stored scoped key invalid: {e}"))?;
        out.push(key);
    }
    Ok(out)
}

pub fn ensure_fts_populated(conn: &Connection) -> Result<(), String> {
    let doc_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM issue_documents WHERE deleted=0",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("could not count documents for FTS sync: {e}"))?;
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM issue_fts", [], |row| row.get(0))
        .map_err(|e| format!("could not count FTS rows: {e}"))?;
    if doc_count != fts_count {
        conn.execute_batch(
            "DELETE FROM issue_fts; INSERT INTO issue_fts(rowid, title, body) SELECT rowid, title, body FROM issue_documents WHERE deleted=0;",
        )
        .map_err(|e| format!("could not rebuild FTS index: {e}"))?;
    }
    Ok(())
}

pub fn load_chunks(conn: &Connection, key: &IssueIndexKey) -> Result<Vec<IssueIndexChunk>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT ordinal, text, byte_start, byte_end, hash FROM issue_chunks \
             WHERE source=?1 AND project=?2 AND external_id=?3 ORDER BY ordinal ASC",
        )
        .map_err(|e| format!("could not prepare chunk load: {e}"))?;
    let rows = stmt
        .query_map(params![key.source, key.project, key.external_id], |row| {
            Ok(IssueIndexChunk {
                ordinal: row.get::<_, i64>(0)? as usize,
                text: row.get(1)?,
                byte_start: row.get::<_, i64>(2)? as usize,
                byte_end: row.get::<_, i64>(3)? as usize,
                hash: row.get(4)?,
            })
        })
        .map_err(|e| format!("could not read chunks: {e}"))?;
    let mut chunks = Vec::new();
    for r in rows {
        chunks.push(r.map_err(|e| format!("could not decode chunk row: {e}"))?);
    }
    Ok(chunks)
}

pub fn doc_from_row(
    conn: &Connection,
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
    let key = IssueIndexKey::new(source, project, external_id)
        .map_err(|e| format!("stored key is invalid: {e}"))?;
    let chunks = load_chunks(conn, &key)?;
    let deleted_flag = deleted != 0;
    Ok(IssueIndexDocument {
        key,
        issue_number: issue_number as u64,
        title,
        body,
        state,
        url,
        provider_updated_at,
        indexed_at,
        content_hash,
        deleted: deleted_flag,
        deleted_at,
        chunks: if deleted_flag { Vec::new() } else { chunks },
    })
}

pub fn create_private_dir(path: &Path) -> Result<(), String> {
    let existed = path.exists();
    fs::create_dir_all(path).map_err(|e| format!("could not create issue index directory: {e}"))?;
    #[cfg(unix)]
    {
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("could not secure issue index directory: {e}"))?;
        }
    }
    Ok(())
}
pub fn project_dirs_index_path() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("com", "Cloud1ful", "phasegent")
        .ok_or_else(|| "could not resolve phasegent config directory".to_owned())?;
    Ok(dirs.config_dir().join(DB_FILENAME_INDEX))
}
