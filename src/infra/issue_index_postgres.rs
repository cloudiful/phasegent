//! PostgreSQL backend for the provider-neutral issue index.
//!
//! Only index tables live in PostgreSQL; credentials, provider config,
//! timers, and local settings remain in `phasegent.sqlite3`. The schema
//! is sourced from `migrations/pg/0001_issue_index.sql` and auto-applied
//! on open through a version-tracked embedded migration (see
//! `apply_embedded_migrations`). Runtime `sqlx::query` APIs with bound
//! values are used throughout; compile-time `query!` macros are avoided
//! because the repo has no offline query metadata/live DB for `cargo check`.

#[cfg(feature = "postgres")]
use crate::providers::index::ISSUE_INDEX_SEARCH_MAX_LIMIT;
use crate::providers::index::{IssueIndexDocument, IssueIndexKey, IssueIndexListOptions};
#[cfg(feature = "postgres")]
use crate::providers::index_store::IssueIndexSearchItem;
use crate::providers::index_store::IssueIndexSearchResult;
use async_trait::async_trait;

#[cfg(feature = "postgres")]
use sqlx::postgres::PgPoolOptions;
#[cfg(feature = "postgres")]
use sqlx::{PgPool, Row};

#[cfg(feature = "postgres")]
pub struct PostgresIssueIndex {
    pool: PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresIssueIndex {
    pub async fn open(url: &str) -> Result<Self, String> {
        // Never include the raw URL in errors.
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(url)
            .await
            .map_err(|_| "could not connect to PostgreSQL index database".to_owned())?;
        // Auto-apply the checked-in migration with version tracking.
        // The migration file is the single source of truth; no ad-hoc
        // schema string duplicates it. Runtime `sqlx::query` calls use
        // bound values only (never interpolated user input); compile-time
        // `query!`/`query_file!` macros are avoided because the repo has
        // no offline query metadata/live DB for `cargo check`.
        apply_embedded_migrations(&pool)
            .await
            .map_err(|_| "could not migrate PostgreSQL index database".to_owned())?;
        Ok(Self { pool })
    }

    async fn load_chunks(
        &self,
        key: &IssueIndexKey,
    ) -> Result<Vec<crate::providers::index::IssueIndexChunk>, String> {
        let rows = sqlx::query(
            "SELECT ordinal, text, byte_start, byte_end, hash FROM issue_chunks \
             WHERE source=$1 AND project=$2 AND external_id=$3 ORDER BY ordinal ASC",
        )
        .bind(&key.source)
        .bind(&key.project)
        .bind(&key.external_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| "could not read chunks".to_owned())?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let ordinal: i32 = row
                .try_get("ordinal")
                .map_err(|_| "could not decode chunk row".to_owned())?;
            let text: String = row
                .try_get("text")
                .map_err(|_| "could not decode chunk row".to_owned())?;
            let byte_start: i32 = row
                .try_get("byte_start")
                .map_err(|_| "could not decode chunk row".to_owned())?;
            let byte_end: i32 = row
                .try_get("byte_end")
                .map_err(|_| "could not decode chunk row".to_owned())?;
            let hash: String = row
                .try_get("hash")
                .map_err(|_| "could not decode chunk row".to_owned())?;
            out.push(crate::providers::index::IssueIndexChunk {
                ordinal: ordinal as usize,
                text,
                byte_start: byte_start as usize,
                byte_end: byte_end as usize,
                hash,
            });
        }
        Ok(out)
    }

    async fn doc_from_row(
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
        deleted: bool,
        deleted_at: Option<i64>,
    ) -> Result<IssueIndexDocument, String> {
        let key = IssueIndexKey::new(source, project, external_id)
            .map_err(|e| format!("stored key is invalid: {e}"))?;
        let chunks = if deleted {
            Vec::new()
        } else {
            self.load_chunks(&key).await?
        };
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
            deleted,
            deleted_at,
            chunks: if deleted { Vec::new() } else { chunks },
        })
    }
}

#[cfg(feature = "postgres")]
#[async_trait(?Send)]
impl crate::providers::index::IssueIndexStore for PostgresIssueIndex {
    async fn upsert(&self, doc: &IssueIndexDocument) -> Result<(), String> {
        doc.validate()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "could not begin index upsert".to_owned())?;
        sqlx::query(
            "INSERT INTO issue_documents \
             (source, project, external_id, issue_number, title, body, state, url, provider_updated_at, indexed_at, content_hash, deleted, deleted_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,FALSE,NULL) \
             ON CONFLICT (source, project, external_id) DO UPDATE SET \
                issue_number=EXCLUDED.issue_number, title=EXCLUDED.title, body=EXCLUDED.body, \
                state=EXCLUDED.state, url=EXCLUDED.url, provider_updated_at=EXCLUDED.provider_updated_at, \
                indexed_at=EXCLUDED.indexed_at, content_hash=EXCLUDED.content_hash, deleted=FALSE, deleted_at=NULL",
        )
        .bind(&doc.key.source)
        .bind(&doc.key.project)
        .bind(&doc.key.external_id)
        .bind(doc.issue_number as i64)
        .bind(&doc.title)
        .bind(&doc.body)
        .bind(&doc.state)
        .bind(&doc.url)
        .bind(doc.provider_updated_at)
        .bind(doc.indexed_at)
        .bind(&doc.content_hash)
        .execute(&mut *tx)
        .await
        .map_err(|_| "could not upsert document".to_owned())?;

        sqlx::query("DELETE FROM issue_chunks WHERE source=$1 AND project=$2 AND external_id=$3")
            .bind(&doc.key.source)
            .bind(&doc.key.project)
            .bind(&doc.key.external_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| "could not clear old chunks".to_owned())?;

        for c in &doc.chunks {
            sqlx::query(
                "INSERT INTO issue_chunks (source, project, external_id, ordinal, text, byte_start, byte_end, hash) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            )
            .bind(&doc.key.source)
            .bind(&doc.key.project)
            .bind(&doc.key.external_id)
            .bind(c.ordinal as i32)
            .bind(&c.text)
            .bind(c.byte_start as i32)
            .bind(c.byte_end as i32)
            .bind(&c.hash)
            .execute(&mut *tx)
            .await
            .map_err(|_| format!("could not insert chunk {}", c.ordinal))?;
        }

        tx.commit()
            .await
            .map_err(|_| "could not commit index upsert".to_owned())?;
        Ok(())
    }

    async fn get(&self, key: &IssueIndexKey) -> Result<Option<IssueIndexDocument>, String> {
        key.validate()?;
        let row = sqlx::query(
            "SELECT source, project, external_id, issue_number, title, body, state, url, provider_updated_at, indexed_at, content_hash, deleted, deleted_at \
             FROM issue_documents WHERE source=$1 AND project=$2 AND external_id=$3",
        )
        .bind(&key.source)
        .bind(&key.project)
        .bind(&key.external_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| "could not read document".to_owned())?;

        match row {
            None => Ok(None),
            Some(r) => {
                let source: String = r
                    .try_get("source")
                    .map_err(|_| "could not decode row".to_owned())?;
                let project: String = r
                    .try_get("project")
                    .map_err(|_| "could not decode row".to_owned())?;
                let external_id: String = r
                    .try_get("external_id")
                    .map_err(|_| "could not decode row".to_owned())?;
                let issue_number: i64 = r
                    .try_get("issue_number")
                    .map_err(|_| "could not decode row".to_owned())?;
                let title: String = r
                    .try_get("title")
                    .map_err(|_| "could not decode row".to_owned())?;
                let body: String = r
                    .try_get("body")
                    .map_err(|_| "could not decode row".to_owned())?;
                let state: String = r
                    .try_get("state")
                    .map_err(|_| "could not decode row".to_owned())?;
                let url: Option<String> = r
                    .try_get("url")
                    .map_err(|_| "could not decode row".to_owned())?;
                let provider_updated_at: Option<i64> = r
                    .try_get("provider_updated_at")
                    .map_err(|_| "could not decode row".to_owned())?;
                let indexed_at: i64 = r
                    .try_get("indexed_at")
                    .map_err(|_| "could not decode row".to_owned())?;
                let content_hash: String = r
                    .try_get("content_hash")
                    .map_err(|_| "could not decode row".to_owned())?;
                let deleted: bool = r
                    .try_get("deleted")
                    .map_err(|_| "could not decode row".to_owned())?;
                let deleted_at: Option<i64> = r
                    .try_get("deleted_at")
                    .map_err(|_| "could not decode row".to_owned())?;
                let doc = self
                    .doc_from_row(
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
                    .await?;
                Ok(Some(doc))
            }
        }
    }

    async fn list(&self, opts: &IssueIndexListOptions) -> Result<Vec<IssueIndexDocument>, String> {
        if opts.limit == 0 || opts.limit > crate::providers::index::ISSUE_INDEX_MAX_LIST_LIMIT {
            return Err(format!(
                "list limit must be between 1 and {}",
                crate::providers::index::ISSUE_INDEX_MAX_LIST_LIMIT
            ));
        }
        let rows = sqlx::query(
            "SELECT source, project, external_id, issue_number, title, body, state, url, provider_updated_at, indexed_at, content_hash, deleted, deleted_at \
             FROM issue_documents WHERE deleted=FALSE ORDER BY source ASC, project ASC, external_id ASC LIMIT $1 OFFSET $2",
        )
        .bind(opts.limit as i64)
        .bind(opts.offset as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| "could not read document list".to_owned())?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let source: String = r
                .try_get("source")
                .map_err(|_| "could not decode list row".to_owned())?;
            let project: String = r
                .try_get("project")
                .map_err(|_| "could not decode list row".to_owned())?;
            let external_id: String = r
                .try_get("external_id")
                .map_err(|_| "could not decode list row".to_owned())?;
            let issue_number: i64 = r
                .try_get("issue_number")
                .map_err(|_| "could not decode list row".to_owned())?;
            let title: String = r
                .try_get("title")
                .map_err(|_| "could not decode list row".to_owned())?;
            let body: String = r
                .try_get("body")
                .map_err(|_| "could not decode list row".to_owned())?;
            let state: String = r
                .try_get("state")
                .map_err(|_| "could not decode list row".to_owned())?;
            let url: Option<String> = r
                .try_get("url")
                .map_err(|_| "could not decode list row".to_owned())?;
            let provider_updated_at: Option<i64> = r
                .try_get("provider_updated_at")
                .map_err(|_| "could not decode list row".to_owned())?;
            let indexed_at: i64 = r
                .try_get("indexed_at")
                .map_err(|_| "could not decode list row".to_owned())?;
            let content_hash: String = r
                .try_get("content_hash")
                .map_err(|_| "could not decode list row".to_owned())?;
            let deleted: bool = r
                .try_get("deleted")
                .map_err(|_| "could not decode list row".to_owned())?;
            let deleted_at: Option<i64> = r
                .try_get("deleted_at")
                .map_err(|_| "could not decode list row".to_owned())?;
            out.push(
                self.doc_from_row(
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
                .await?,
            );
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
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "could not begin tombstone".to_owned())?;
        sqlx::query(
            "INSERT INTO issue_documents \
             (source, project, external_id, issue_number, title, body, state, url, provider_updated_at, indexed_at, content_hash, deleted, deleted_at) \
             VALUES ($1,$2,$3,0,$4,$5,$6,NULL,NULL,$7,$8,TRUE,$7) \
             ON CONFLICT (source, project, external_id) DO UPDATE SET deleted=TRUE, deleted_at=EXCLUDED.deleted_at, indexed_at=EXCLUDED.indexed_at",
        )
        .bind(&key.source)
        .bind(&key.project)
        .bind(&key.external_id)
        .bind(placeholder_title)
        .bind(placeholder_body)
        .bind(placeholder_state)
        .bind(indexed_at)
        .bind(&placeholder_hash)
        .execute(&mut *tx)
        .await
        .map_err(|_| "could not tombstone document".to_owned())?;

        sqlx::query("DELETE FROM issue_chunks WHERE source=$1 AND project=$2 AND external_id=$3")
            .bind(&key.source)
            .bind(&key.project)
            .bind(&key.external_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| "could not delete tombstoned chunks".to_owned())?;

        tx.commit()
            .await
            .map_err(|_| "could not commit tombstone".to_owned())?;
        Ok(())
    }

    async fn lexical_search(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        include_body: bool,
    ) -> Result<IssueIndexSearchResult, String> {
        self.lexical_search_scoped(
            query,
            limit,
            offset,
            include_body,
            &crate::providers::index::LexicalScope::global(),
        )
        .await
    }

    async fn lexical_search_scoped(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        include_body: bool,
        scope: &crate::providers::index::LexicalScope,
    ) -> Result<IssueIndexSearchResult, String> {
        if limit == 0 || limit > ISSUE_INDEX_SEARCH_MAX_LIMIT {
            return Err(format!(
                "search limit must be between 1 and {}",
                ISSUE_INDEX_SEARCH_MAX_LIMIT
            ));
        }
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(
                "issue index search requires --query TEXT (empty queries are rejected)".to_owned(),
            );
        }
        let has_scope = scope.source.is_some() && scope.project.is_some();
        let has_state = matches!(scope.state.as_deref(), Some("open") | Some("closed"));
        // Bounded count with optional scope/state filters.
        let total: i64 = match (has_scope, has_state) {
            (false, false) => sqlx::query_scalar(
                "SELECT COUNT(*) FROM issue_documents WHERE search_vector @@ plainto_tsquery('english', $1) AND deleted=FALSE",
            )
            .bind(trimmed)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| "could not count lexical results".to_owned())?,
            (false, true) => {
                let state = scope.state.as_deref().unwrap_or_default();
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM issue_documents WHERE search_vector @@ plainto_tsquery('english', $1) AND deleted=FALSE AND state=$2",
                )
                .bind(trimmed)
                .bind(state)
                .fetch_one(&self.pool)
                .await
                .map_err(|_| "could not count lexical results".to_owned())?
            }
            (true, false) => {
                let source = scope.source.as_deref().unwrap_or_default();
                let project = scope.project.as_deref().unwrap_or_default();
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM issue_documents WHERE search_vector @@ plainto_tsquery('english', $1) AND deleted=FALSE AND source=$2 AND project=$3",
                )
                .bind(trimmed)
                .bind(source)
                .bind(project)
                .fetch_one(&self.pool)
                .await
                .map_err(|_| "could not count lexical results".to_owned())?
            }
            (true, true) => {
                let source = scope.source.as_deref().unwrap_or_default();
                let project = scope.project.as_deref().unwrap_or_default();
                let state = scope.state.as_deref().unwrap_or_default();
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM issue_documents WHERE search_vector @@ plainto_tsquery('english', $1) AND deleted=FALSE AND source=$2 AND project=$3 AND state=$4",
                )
                .bind(trimmed)
                .bind(source)
                .bind(project)
                .bind(state)
                .fetch_one(&self.pool)
                .await
                .map_err(|_| "could not count lexical results".to_owned())?
            }
        };
        let total_count = total as usize;

        let rows = match (has_scope, has_state) {
            (false, false) => sqlx::query(
                "SELECT source, project, external_id, issue_number, title, body, state, url, provider_updated_at, indexed_at, content_hash, deleted, deleted_at, \
                 ts_rank(search_vector, plainto_tsquery('english', $1)) AS rank \
                 FROM issue_documents \
                 WHERE search_vector @@ plainto_tsquery('english', $1) AND deleted=FALSE \
                 ORDER BY rank DESC, source ASC, project ASC, external_id ASC \
                 LIMIT $2 OFFSET $3",
            )
            .bind(trimmed)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| "could not execute lexical search".to_owned())?,
            (false, true) => {
                let state = scope.state.as_deref().unwrap_or_default();
                sqlx::query(
                    "SELECT source, project, external_id, issue_number, title, body, state, url, provider_updated_at, indexed_at, content_hash, deleted, deleted_at, \
                     ts_rank(search_vector, plainto_tsquery('english', $1)) AS rank \
                     FROM issue_documents \
                     WHERE search_vector @@ plainto_tsquery('english', $1) AND deleted=FALSE AND state=$2 \
                     ORDER BY rank DESC, source ASC, project ASC, external_id ASC \
                     LIMIT $3 OFFSET $4",
                )
                .bind(trimmed)
                .bind(state)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "could not execute lexical search".to_owned())?
            }
            (true, false) => {
                let source = scope.source.as_deref().unwrap_or_default();
                let project = scope.project.as_deref().unwrap_or_default();
                sqlx::query(
                    "SELECT source, project, external_id, issue_number, title, body, state, url, provider_updated_at, indexed_at, content_hash, deleted, deleted_at, \
                     ts_rank(search_vector, plainto_tsquery('english', $1)) AS rank \
                     FROM issue_documents \
                     WHERE search_vector @@ plainto_tsquery('english', $1) AND deleted=FALSE AND source=$2 AND project=$3 \
                     ORDER BY rank DESC, source ASC, project ASC, external_id ASC \
                     LIMIT $4 OFFSET $5",
                )
                .bind(trimmed)
                .bind(source)
                .bind(project)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "could not execute lexical search".to_owned())?
            }
            (true, true) => {
                let source = scope.source.as_deref().unwrap_or_default();
                let project = scope.project.as_deref().unwrap_or_default();
                let state = scope.state.as_deref().unwrap_or_default();
                sqlx::query(
                    "SELECT source, project, external_id, issue_number, title, body, state, url, provider_updated_at, indexed_at, content_hash, deleted, deleted_at, \
                     ts_rank(search_vector, plainto_tsquery('english', $1)) AS rank \
                     FROM issue_documents \
                     WHERE search_vector @@ plainto_tsquery('english', $1) AND deleted=FALSE AND source=$2 AND project=$3 AND state=$4 \
                     ORDER BY rank DESC, source ASC, project ASC, external_id ASC \
                     LIMIT $5 OFFSET $6",
                )
                .bind(trimmed)
                .bind(source)
                .bind(project)
                .bind(state)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "could not execute lexical search".to_owned())?
            }
        };

        let mut items = Vec::with_capacity(rows.len());
        for r in rows {
            let source: String = r
                .try_get("source")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let project: String = r
                .try_get("project")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let external_id: String = r
                .try_get("external_id")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let issue_number: i64 = r
                .try_get("issue_number")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let title: String = r
                .try_get("title")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let body: String = r
                .try_get("body")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let state: String = r
                .try_get("state")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let url: Option<String> = r
                .try_get("url")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let provider_updated_at: Option<i64> = r
                .try_get("provider_updated_at")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let indexed_at: i64 = r
                .try_get("indexed_at")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let content_hash: String = r
                .try_get("content_hash")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let deleted: bool = r
                .try_get("deleted")
                .map_err(|_| "could not decode lexical row".to_owned())?;
            let deleted_at: Option<i64> = r
                .try_get("deleted_at")
                .map_err(|_| "could not decode lexical row".to_owned())?;
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
                deleted,
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

    async fn list_active_keys_for_scope(
        &self,
        source: &str,
        project: &str,
    ) -> Result<Vec<IssueIndexKey>, String> {
        let rows = sqlx::query(
            "SELECT source, project, external_id FROM issue_documents \
             WHERE source=$1 AND project=$2 AND deleted=FALSE \
             ORDER BY source ASC, project ASC, external_id ASC",
        )
        .bind(source)
        .bind(project)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| "could not query scoped active keys".to_owned())?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let source: String = r
                .try_get("source")
                .map_err(|_| "could not decode scoped key row".to_owned())?;
            let project: String = r
                .try_get("project")
                .map_err(|_| "could not decode scoped key row".to_owned())?;
            let external_id: String = r
                .try_get("external_id")
                .map_err(|_| "could not decode scoped key row".to_owned())?;
            let key = IssueIndexKey::new(source, project, external_id)
                .map_err(|e| format!("stored scoped key invalid: {e}"))?;
            out.push(key);
        }
        Ok(out)
    }
}

/// Embedded migration SQL for the PostgreSQL index backend.
/// The file is the single source of truth for the index schema
/// (`tsvector` + GIN, PK, cascade, tombstones); no Rust string duplicates it.
/// `include_str!` embeds it at compile time so default SQLite builds still
/// compile without the `postgres` feature (the function below is
/// `#[cfg(feature = "postgres")]`-gated, but the file must exist).
#[cfg(feature = "postgres")]
const PG_MIGRATION_SQL: &str = include_str!("../../migrations/pg/0001_issue_index.sql");

/// Version-tracked auto-migration for the PostgreSQL index backend.
///
/// Applies `PG_MIGRATION_SQL` once and records version 1 in
/// `_issue_index_migrations`. Subsequent opens are no-ops. Failures
/// surface as migration errors (never silent fallback). This provides
/// the same guarantee as `sqlx::migrate!` (embedded SQL + tracking)
/// using only runtime `sqlx::query` APIs: `sqlx::migrate!` would require
/// the `migrate` Cargo feature, which currently cannot be resolved in
/// this environment (it pulls `sqlx-sqlite`/`libsqlite3-sys 0.30` that
/// conflicts with `rusqlite`'s `libsqlite3-sys 0.38` via the configured
/// registry), and compile-time `query_file!` would require offline query
/// metadata/live DB that the repo does not provide.
#[cfg(feature = "postgres")]
async fn apply_embedded_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    // Tracking table first so the check below is always valid.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS _issue_index_migrations \
         (version BIGINT PRIMARY KEY, description TEXT NOT NULL, applied_at BIGINT NOT NULL)",
    )
    .execute(pool)
    .await?;
    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _issue_index_migrations WHERE version=$1")
            .bind(1_i64)
            .fetch_optional(pool)
            .await?;
    if applied.is_some() {
        return Ok(());
    }
    // Multi-statement migration (tables, GIN index, trigger function).
    sqlx::raw_sql(PG_MIGRATION_SQL).execute(pool).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_700_000_000);
    sqlx::query(
        "INSERT INTO _issue_index_migrations (version, description, applied_at) \
         VALUES ($1,$2,$3) ON CONFLICT (version) DO NOTHING",
    )
    .bind(1_i64)
    .bind("issue_index")
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(not(feature = "postgres"))]
pub struct PostgresIssueIndex;

#[cfg(not(feature = "postgres"))]
impl PostgresIssueIndex {
    pub async fn open(_url: &str) -> Result<Self, String> {
        Err("postgres index support is not enabled; rebuild with --features postgres".to_owned())
    }
}

#[cfg(not(feature = "postgres"))]
#[async_trait(?Send)]
impl crate::providers::index::IssueIndexStore for PostgresIssueIndex {
    async fn upsert(&self, _doc: &IssueIndexDocument) -> Result<(), String> {
        Err("postgres index support is not enabled; rebuild with --features postgres".to_owned())
    }
    async fn get(&self, _key: &IssueIndexKey) -> Result<Option<IssueIndexDocument>, String> {
        Err("postgres index support is not enabled; rebuild with --features postgres".to_owned())
    }
    async fn list(&self, _opts: &IssueIndexListOptions) -> Result<Vec<IssueIndexDocument>, String> {
        Err("postgres index support is not enabled; rebuild with --features postgres".to_owned())
    }
    async fn tombstone(&self, _key: &IssueIndexKey, _indexed_at: i64) -> Result<(), String> {
        Err("postgres index support is not enabled; rebuild with --features postgres".to_owned())
    }
    async fn lexical_search(
        &self,
        _query: &str,
        _limit: usize,
        _offset: usize,
        _include_body: bool,
    ) -> Result<IssueIndexSearchResult, String> {
        Err("postgres index support is not enabled; rebuild with --features postgres".to_owned())
    }
    async fn lexical_search_scoped(
        &self,
        _query: &str,
        _limit: usize,
        _offset: usize,
        _include_body: bool,
        _scope: &crate::providers::index::LexicalScope,
    ) -> Result<IssueIndexSearchResult, String> {
        Err("postgres index support is not enabled; rebuild with --features postgres".to_owned())
    }
    async fn list_active_keys_for_scope(
        &self,
        _source: &str,
        _project: &str,
    ) -> Result<Vec<IssueIndexKey>, String> {
        Err("postgres index support is not enabled; rebuild with --features postgres".to_owned())
    }
}
