use rusqlite::Connection;

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
