//! FTS backfill and private-path helpers for the SQLite issue index.
//! Extracted to keep `issue_index.rs` below size thresholds.

use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

use crate::infra::issue_index_schema::DB_FILENAME_INDEX;
use directories::ProjectDirs;

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
