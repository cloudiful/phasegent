//! Lexical search over the independent SQLite FTS5 index.
//! Extracted to keep `issue_index.rs` cohesive and below thresholds.

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
/// Validate and normalize a raw user query for lexical search.
/// Returns the escaped FTS query or a config error.
pub fn normalize_query(raw: &str) -> Result<String, String> {
    escape_fts_query(raw)
}

/// Perform the actual FTS lookup. Caller validates limit/offset and holds
/// no transaction; this runs two bounded queries (count + page).
pub fn lexical_search_inner(
    conn: &Connection,
    escaped_query: &str,
    limit: usize,
    offset: usize,
    include_body: bool,
) -> Result<IssueIndexSearchResult, String> {
    lexical_search_scoped_inner(
        conn,
        escaped_query,
        limit,
        offset,
        include_body,
        &crate::providers::index::LexicalScope::global(),
    )
}

/// Scoped FTS lookup for transparent fallback. Filters by `source`/
/// `project` when both are present and by `state` when it is
/// `Some(open|closed)`; a global scope behaves exactly like
/// [`lexical_search_inner`]. Deterministic ordering and bounded
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
    return lexical_search_scoped_page_inner(
        conn,
        escaped_query,
        limit,
        offset,
        include_body,
        scope,
        total_count,
    );
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

#[cfg(test)]
mod tests {
    use crate::infra::issue_index::SqliteIssueIndex;
    use crate::infra::issue_index_backend::block_on;
    use crate::providers::index::{IssueIndexDocument, IssueIndexKey, IssueIndexStore};
    use std::fs;
    use std::path::PathBuf;

    fn tmp_index(label: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "phasegent-index-search-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("phasegent-index.sqlite3");
        (dir, path)
    }

    #[test]
    fn fts_term_unicode_and_invalid_query_behavior() {
        let (dir, path) = tmp_index("unicode");
        let idx = SqliteIssueIndex::open_at(&path).unwrap();
        let key1 = IssueIndexKey::new("forgejo", "owner/repo", "1").unwrap();
        let doc1 = IssueIndexDocument::new(
            key1,
            1,
            "hello world".into(),
            "body with 汉字 😀".into(),
            "open".into(),
            None,
            None,
            1_700_000_000,
        )
        .unwrap();
        block_on(idx.upsert(&doc1)).unwrap();
        // ASCII term
        let res = block_on(idx.lexical_search("hello", 10, 0, false)).unwrap();
        assert_eq!(res.total_count, 1);
        assert_eq!(res.items[0].title, "hello world");
        assert!(res.items[0].body.is_none());
        // Unicode term - should not crash, may match depending on tokenizer
        let res = block_on(idx.lexical_search("汉字", 10, 0, true)).unwrap();
        // Either 1 if tokenizer treats CJK as single token, or 0 if split; both are acceptable as long as no crash
        assert!(res.total_count <= 1);
        if res.total_count == 1 {
            assert!(res.items[0].body.is_some());
        }
        // Emoji term - same: should not crash
        let res = block_on(idx.lexical_search("😀", 10, 0, false)).unwrap();
        assert!(res.total_count <= 1);
        // Invalid/empty query must be config error, not panic
        assert!(block_on(idx.lexical_search("   ", 10, 0, false)).is_err());
        assert!(block_on(idx.lexical_search("", 10, 0, false)).is_err());
        // Malformed FTS syntax should be escaped, not crash: e.g., `OR` or `"`.
        let _ = block_on(idx.lexical_search("OR", 10, 0, false)).unwrap();
        let _ = block_on(idx.lexical_search("\"hello", 10, 0, false)).unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn output_compactness_and_truncation() {
        let (dir, path) = tmp_index("compact");
        let idx = SqliteIssueIndex::open_at(&path).unwrap();
        let long_body = "a".repeat(crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES + 100);
        let key = IssueIndexKey::new("forgejo", "owner/repo", "2").unwrap();
        let doc = IssueIndexDocument::new(
            key,
            2,
            "long".into(),
            long_body.clone(),
            "open".into(),
            None,
            None,
            1_700_000_001,
        )
        .unwrap();
        block_on(idx.upsert(&doc)).unwrap();
        // Without include_body, body omitted and not truncated
        let res = block_on(idx.lexical_search("long", 10, 0, false)).unwrap();
        assert!(res.items[0].body.is_none());
        assert!(res.items[0].body_truncated.is_none());
        // With include_body, body capped to 8192 and marked truncated
        let res = block_on(idx.lexical_search("long", 10, 0, true)).unwrap();
        assert_eq!(res.items[0].body_truncated, Some(true));
        assert_eq!(
            res.items[0].body.as_ref().unwrap().len(),
            crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES
        );
        assert!(
            res.items[0]
                .body
                .as_ref()
                .unwrap()
                .is_char_boundary(res.items[0].body.as_ref().unwrap().len())
        );
        // Short body not truncated
        let key2 = IssueIndexKey::new("forgejo", "owner/repo", "3").unwrap();
        let doc2 = IssueIndexDocument::new(
            key2,
            3,
            "short".into(),
            "short".into(),
            "open".into(),
            None,
            None,
            1_700_000_002,
        )
        .unwrap();
        block_on(idx.upsert(&doc2)).unwrap();
        let res = block_on(idx.lexical_search("short", 10, 0, true)).unwrap();
        assert_eq!(res.items[0].body_truncated, Some(false));
        assert_eq!(res.items[0].body.as_deref(), Some("short"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn fts_round_trip_after_upsert() {
        let (dir, path) = tmp_index("roundtrip");
        let idx = SqliteIssueIndex::open_at(&path).unwrap();
        let key = IssueIndexKey::new("redmine", "proj1", "99").unwrap();
        let doc = IssueIndexDocument::new(
            key.clone(),
            99,
            "alpha".into(),
            "beta body".into(),
            "open".into(),
            None,
            None,
            1_700_000_100,
        )
        .unwrap();
        block_on(idx.upsert(&doc)).unwrap();
        assert_eq!(
            block_on(idx.lexical_search("alpha", 10, 0, false))
                .unwrap()
                .total_count,
            1
        );
        // Replacement atomically updates FTS.
        let doc2 = IssueIndexDocument::new(
            key.clone(),
            99,
            "alpha".into(),
            "revived body".into(),
            "open".into(),
            None,
            None,
            1_700_000_300,
        )
        .unwrap();
        block_on(idx.upsert(&doc2)).unwrap();
        assert_eq!(
            block_on(idx.lexical_search("alpha", 10, 0, false))
                .unwrap()
                .total_count,
            1
        );
        assert_eq!(
            block_on(idx.lexical_search("revived", 10, 0, false))
                .unwrap()
                .total_count,
            1
        );
        // Old-only term no longer matches after atomic replacement.
        assert_eq!(
            block_on(idx.lexical_search("beta", 10, 0, false))
                .unwrap()
                .total_count,
            0
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stable_deterministic_ordering_for_ties() {
        let (dir, path) = tmp_index("ordering");
        let idx = SqliteIssueIndex::open_at(&path).unwrap();
        // Two docs with same content -> same rank, should order by source/project/external_id
        let k1 = IssueIndexKey::new("forgejo", "owner/repo", "10").unwrap();
        let k2 = IssueIndexKey::new("forgejo", "owner/repo", "2").unwrap();
        let d1 = IssueIndexDocument::new(
            k1,
            10,
            "same".into(),
            "same body".into(),
            "open".into(),
            None,
            None,
            1_700_000_000,
        )
        .unwrap();
        let d2 = IssueIndexDocument::new(
            k2,
            2,
            "same".into(),
            "same body".into(),
            "open".into(),
            None,
            None,
            1_700_000_001,
        )
        .unwrap();
        block_on(idx.upsert(&d1)).unwrap();
        block_on(idx.upsert(&d2)).unwrap();
        let res = block_on(idx.lexical_search("same", 10, 0, false)).unwrap();
        assert_eq!(res.items.len(), 2);
        // Ordered by external_id asc as tie-breaker: "10" vs "2" lexical => "10" < "2"
        assert_eq!(res.items[0].external_id, "10");
        assert_eq!(res.items[1].external_id, "2");
        // Second search must give same order (deterministic)
        let res2 = block_on(idx.lexical_search("same", 10, 0, false)).unwrap();
        assert_eq!(res.items[0].external_id, res2.items[0].external_id);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn lexical_search_is_local_only_and_no_network_needed() {
        // Ensure lexical search works without any provider env or network.
        // This test never touches Storage or provider config; it only uses the temp index file.
        let (dir, path) = tmp_index("localonly");
        // Guard against accidental real DB: use temp path via env guard
        let _guard = crate::infra::storage::test_support::EnvGuard::set(
            "PHASEGENT_INDEX_DB_PATH",
            path.to_str().unwrap(),
        );
        let idx = SqliteIssueIndex::open().unwrap();
        let key = IssueIndexKey::new("gitlab", "42", "1").unwrap();
        let doc = IssueIndexDocument::new(
            key,
            1,
            "local".into(),
            "no network needed".into(),
            "open".into(),
            None,
            None,
            1_700_000_000,
        )
        .unwrap();
        block_on(idx.upsert(&doc)).unwrap();
        let res = block_on(idx.lexical_search("local", 10, 0, false)).unwrap();
        assert_eq!(res.total_count, 1);
        assert_eq!(res.items[0].source, "gitlab");
        let _ = fs::remove_dir_all(dir);
    }
}
