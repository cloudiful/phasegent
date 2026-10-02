use rusqlite::{Connection, params};

use crate::providers::index::{IssueIndexDocument, IssueIndexKey};
use crate::providers::index_store::{IssueIndexSearchItem, IssueIndexSearchResult};

/// Escape user input for FTS5 MATCH so ordinary terms and Unicode work
/// without allowing malformed syntax to crash the command.
/// Each whitespace-separated token is wrapped in double quotes after
/// doubling internal quotes. Returns a config error for empty/whitespace.
pub fn escape_fts_query(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("lexical search requires a non-empty query".to_owned());
    }
    // Split on Unicode whitespace, filter empties, escape each token.
    let mut escaped_tokens = Vec::new();
    for token in trimmed.split_whitespace() {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        // FTS5 phrase escaping: double any embedded double quote then wrap.
        let escaped = t.replace('"', "\"\"");
        escaped_tokens.push(format!("\"{escaped}\""));
    }
    if escaped_tokens.is_empty() {
        return Err("lexical search requires a non-empty query".to_owned());
    }
    // Join with space => implicit AND between terms.
    Ok(escaped_tokens.join(" "))
}

/// Scoped FTS lookup for transparent fallback. Filters by `source`/
/// `project` when both are present and by `state` when it is
/// `Some(open|closed)`; a global scope behaves exactly like an
/// unfiltered search. Deterministic ordering and bounded
/// pagination are preserved.
pub fn lexical_search_scoped_inner(
    conn: &Connection,
    escaped_query: &str,
    limit: usize,
    offset: usize,
    include_body: bool,
    scope: &crate::providers::index::LexicalScope,
) -> Result<IssueIndexSearchResult, String> {
    let has_scope = scope.source.is_some() && scope.project.is_some();
    let has_state = matches!(scope.state.as_deref(), Some("open") | Some("closed"));
    // Total count of matching non-deleted documents.
    let total_count: i64 = match (has_scope, has_state) {
        (false, false) => conn
            .query_row(
                "SELECT COUNT(*) FROM issue_fts JOIN issue_documents d ON d.rowid = issue_fts.rowid WHERE issue_fts MATCH ?1 AND d.deleted=0",
                params![escaped_query],
                |row| row.get(0),
            )
            .map_err(|e| format!("could not count lexical results: {e}"))?,
        (false, true) => {
            let state = scope.state.as_deref().unwrap_or_default();
            conn.query_row(
                "SELECT COUNT(*) FROM issue_fts JOIN issue_documents d ON d.rowid = issue_fts.rowid WHERE issue_fts MATCH ?1 AND d.deleted=0 AND d.state=?2",
                params![escaped_query, state],
                |row| row.get(0),
            )
            .map_err(|e| format!("could not count lexical results: {e}"))?
        }
        (true, false) => {
            let source = scope.source.as_deref().unwrap_or_default();
            let project = scope.project.as_deref().unwrap_or_default();
            conn.query_row(
                "SELECT COUNT(*) FROM issue_fts JOIN issue_documents d ON d.rowid = issue_fts.rowid WHERE issue_fts MATCH ?1 AND d.deleted=0 AND d.source=?2 AND d.project=?3",
                params![escaped_query, source, project],
                |row| row.get(0),
            )
            .map_err(|e| format!("could not count lexical results: {e}"))?
        }
        (true, true) => {
            let source = scope.source.as_deref().unwrap_or_default();
            let project = scope.project.as_deref().unwrap_or_default();
            let state = scope.state.as_deref().unwrap_or_default();
            conn.query_row(
                "SELECT COUNT(*) FROM issue_fts JOIN issue_documents d ON d.rowid = issue_fts.rowid WHERE issue_fts MATCH ?1 AND d.deleted=0 AND d.source=?2 AND d.project=?3 AND d.state=?4",
                params![escaped_query, source, project, state],
                |row| row.get(0),
            )
            .map_err(|e| format!("could not count lexical results: {e}"))?
        }
    };
    let total_count = total_count as usize;
    lexical_search_scoped_page_inner(
        conn,
        escaped_query,
        limit,
        offset,
        include_body,
        scope,
        total_count,
    )
}

/// Scoped page fetch with explicit filter bindings. Split out so the
/// count query above and the page query below stay in sync.
#[allow(clippy::too_many_arguments)]
fn lexical_search_scoped_page_inner(
    conn: &Connection,
    escaped_query: &str,
    limit: usize,
    offset: usize,
    include_body: bool,
    scope: &crate::providers::index::LexicalScope,
    total_count: usize,
) -> Result<IssueIndexSearchResult, String> {
    let has_scope = scope.source.is_some() && scope.project.is_some();
    let has_state = matches!(scope.state.as_deref(), Some("open") | Some("closed"));
    // Build the page SQL with sequential placeholders so `params!`
    // binding stays positional. Each branch below binds in the same
    // order the placeholders appear.
    let sql = match (has_scope, has_state) {
        (false, false) => String::from(
            "SELECT d.source, d.project, d.external_id, d.issue_number, d.title, d.body, d.state, d.url, d.provider_updated_at, d.indexed_at, d.content_hash, d.deleted, d.deleted_at, rank \
             FROM issue_fts JOIN issue_documents d ON d.rowid = issue_fts.rowid \
             WHERE issue_fts MATCH ?1 AND d.deleted=0 \
             ORDER BY rank, d.source ASC, d.project ASC, d.external_id ASC LIMIT ?2 OFFSET ?3",
        ),
        (false, true) => String::from(
            "SELECT d.source, d.project, d.external_id, d.issue_number, d.title, d.body, d.state, d.url, d.provider_updated_at, d.indexed_at, d.content_hash, d.deleted, d.deleted_at, rank \
             FROM issue_fts JOIN issue_documents d ON d.rowid = issue_fts.rowid \
             WHERE issue_fts MATCH ?1 AND d.deleted=0 AND d.state=?2 \
             ORDER BY rank, d.source ASC, d.project ASC, d.external_id ASC LIMIT ?3 OFFSET ?4",
        ),
        (true, false) => String::from(
            "SELECT d.source, d.project, d.external_id, d.issue_number, d.title, d.body, d.state, d.url, d.provider_updated_at, d.indexed_at, d.content_hash, d.deleted, d.deleted_at, rank \
             FROM issue_fts JOIN issue_documents d ON d.rowid = issue_fts.rowid \
             WHERE issue_fts MATCH ?1 AND d.deleted=0 AND d.source=?2 AND d.project=?3 \
             ORDER BY rank, d.source ASC, d.project ASC, d.external_id ASC LIMIT ?4 OFFSET ?5",
        ),
        (true, true) => String::from(
            "SELECT d.source, d.project, d.external_id, d.issue_number, d.title, d.body, d.state, d.url, d.provider_updated_at, d.indexed_at, d.content_hash, d.deleted, d.deleted_at, rank \
             FROM issue_fts JOIN issue_documents d ON d.rowid = issue_fts.rowid \
             WHERE issue_fts MATCH ?1 AND d.deleted=0 AND d.source=?2 AND d.project=?3 AND d.state=?4 \
             ORDER BY rank, d.source ASC, d.project ASC, d.external_id ASC LIMIT ?5 OFFSET ?6",
        ),
    };
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("could not prepare lexical search: {e}"))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, i64>(11)?,
            row.get::<_, Option<i64>>(12)?,
            row.get::<_, f64>(13)?,
        ))
    };
    let rows = match (has_scope, has_state) {
        (false, false) => stmt
            .query_map(params![escaped_query, limit as i64, offset as i64], map_row)
            .map_err(|e| format!("could not execute lexical search: {e}"))?,
        (true, false) => {
            let source = scope.source.as_deref().unwrap_or_default();
            let project = scope.project.as_deref().unwrap_or_default();
            stmt.query_map(
                params![escaped_query, source, project, limit as i64, offset as i64],
                map_row,
            )
            .map_err(|e| format!("could not execute lexical search: {e}"))?
        }
        (false, true) => {
            let state = scope.state.as_deref().unwrap_or_default();
            stmt.query_map(
                params![escaped_query, state, limit as i64, offset as i64],
                map_row,
            )
            .map_err(|e| format!("could not execute lexical search: {e}"))?
        }
        (true, true) => {
            let source = scope.source.as_deref().unwrap_or_default();
            let project = scope.project.as_deref().unwrap_or_default();
            let state = scope.state.as_deref().unwrap_or_default();
            stmt.query_map(
                params![
                    escaped_query,
                    source,
                    project,
                    state,
                    limit as i64,
                    offset as i64
                ],
                map_row,
            )
            .map_err(|e| format!("could not execute lexical search: {e}"))?
        }
    };
    let mut items = Vec::new();
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
            _rank,
        ) = r.map_err(|e| format!("could not decode lexical row: {e}"))?;
        let key = IssueIndexKey::new(source.clone(), project.clone(), external_id.clone())
            .map_err(|e| format!("stored key invalid during search: {e}"))?;
        let doc = IssueIndexDocument {
            key,
            issue_number: issue_number as u64,
            title,
            body,
            state,
            url,
            provider_updated_at,
            indexed_at,
            content_hash,
            deleted: deleted != 0,
            deleted_at,
            chunks: Vec::new(),
        };
        items.push(IssueIndexSearchItem::from_document(&doc, include_body));
    }
    let has_more = offset + items.len() < total_count;
    Ok(IssueIndexSearchResult {
        items,
        offset,
        limit,
        total_count,
        has_more,
    })
}
