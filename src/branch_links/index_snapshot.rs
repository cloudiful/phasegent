//! Local-only last-known issue-state reads for branch status.
//!
//! Point-reads `(state, indexed_at)` from the SQLite issue index by
//! `(source, project, external_id)` without provider credentials, config
//! resolution, or network access. Anything unreadable — missing file,
//! missing row, PostgreSQL backend, malformed value — projects as unknown
//! (`None`), never as a freeness claim and never as an error.

//! P3 integration API.
#![allow(dead_code)]

use rusqlite::OptionalExtension;

/// State-source label reported alongside projected states.
pub const SNAPSHOT_SOURCE: &str = "local_index";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSnapshot {
    pub state: String,
    pub indexed_at: i64,
}

/// Read one issue's last-known snapshot from the local SQLite index.
/// Returns `None` when the state is unknown for any reason.
pub fn read_snapshot(source: &str, project: &str, external_id: &str) -> Option<IndexSnapshot> {
    if source.trim().is_empty() || project.trim().is_empty() || external_id.trim().is_empty() {
        return None;
    }
    // PostgreSQL-backed deployments have no local snapshot to read;
    // honest unknown beats a networked point-read on a local path.
    if let Ok(raw) = std::env::var("PHASEGENT_INDEX_PG_URL")
        && !raw.trim().is_empty()
    {
        return None;
    }
    let path = index_path()?;
    if !path.is_file() {
        return None;
    }
    let connection =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let row: Option<(String, i64)> = connection
        .query_row(
            "SELECT state, indexed_at FROM issue_documents \
             WHERE source = ?1 AND project = ?2 AND external_id = ?3 AND deleted = 0",
            rusqlite::params![source.trim(), project.trim(), external_id.trim()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    let (state, indexed_at) = row?;
    if state.trim().is_empty() || indexed_at <= 0 {
        return None;
    }
    Some(IndexSnapshot { state, indexed_at })
}

fn index_path() -> Option<std::path::PathBuf> {
    if let Some(raw) = std::env::var_os("PHASEGENT_INDEX_DB_PATH") {
        let path = std::path::PathBuf::from(raw);
        if path.is_absolute() {
            return Some(path);
        }
        return None;
    }
    crate::infra::issue_index::project_dirs_index_path().ok()
}
