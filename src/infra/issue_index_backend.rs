//! Backend selection and opener for the provider-neutral issue index.
//!
//! The index has two mutually exclusive backends: a local SQLite file
//! (`phasegent-index.sqlite3`, default) and a shared PostgreSQL database.
//! Credentials, provider config, and timers remain in SQLite; only the
//! index tables live in PostgreSQL. Selection is URL-driven: a non-empty
//! `PHASEGENT_INDEX_PG_URL` from the environment or persisted global
//! setting selects PostgreSQL, while an absent or blank URL selects
//! SQLite (env overrides persisted, same precedence as other globals).
//! The legacy `PHASEGENT_INDEX_BACKEND` setting is ignored for selection
//! and kept only for compatibility (readable/clearable). A configured
//! PostgreSQL URL that is malformed, unreachable, migration-incompatible,
//! or used without the `postgres` feature fails with a structured config
//! error and never falls back to SQLite. No error, snapshot, or debug
//! output ever prints the URL.

use crate::infra::issue_index::SqliteIssueIndex;
use crate::infra::issue_index_postgres::PostgresIssueIndex;
use crate::infra::storage::Storage;
use crate::providers::index::IssueIndexStore;
use async_trait::async_trait;

/// Backend kind selected from the PostgreSQL URL presence.
/// The legacy `PHASEGENT_INDEX_BACKEND` literal (`sqlite`/`postgres`) is
/// no longer consulted; this enum only reports which backend the URL
/// presence resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexBackendKind {
    Sqlite,
    Postgres,
}

impl IndexBackendKind {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
        }
    }
}

/// Resolved index backend plus the optional PostgreSQL URL (when postgres).
/// The URL value is never included in Debug output.
#[allow(dead_code)]
pub struct IndexBackendConfig {
    pub kind: IndexBackendKind,
    /// Present only when `kind == Postgres` and a URL was supplied via
    /// env or persistent storage.
    pub pg_url_present: bool,
}

/// Enum dispatcher that implements `IssueIndexStore` without requiring a
/// `Send` bound on the future (SQLite's rusqlite connection is `!Send`).
/// Callers hold the enum by `&self` and `await` the trait methods; the
/// CLI layer bridges the sync entry point with a narrowly scoped
/// `block_on` that runs the `!Send` future on a current-thread runtime.
pub enum IssueIndexBackend {
    Sqlite(SqliteIssueIndex),
    Postgres(PostgresIssueIndex),
}

impl IssueIndexBackend {
    /// Open the configured backend. Reads `Storage` from the standard
    /// location (honouring `PHASEGENT_DB_PATH` for tests) and then
    /// delegates to `open_with_storage`.
    pub async fn open() -> Result<Self, String> {
        let storage = Storage::open()?;
        Self::open_with_storage(&storage).await
    }

    /// Testable opener that takes an explicit `Storage` handle so tests
    /// can drive the selector against an isolated temp database without
    /// relying on the global `PHASEGENT_DB_PATH`.
    pub async fn open_with_storage(storage: &Storage) -> Result<Self, String> {
        let kind = resolve_index_backend(storage)?;
        match kind {
            IndexBackendKind::Sqlite => {
                let sqlite = SqliteIssueIndex::open()?;
                Ok(Self::Sqlite(sqlite))
            }
            IndexBackendKind::Postgres => {
                let url = resolve_pg_url(storage)?.ok_or_else(|| {
                    "postgres index backend requires PHASEGENT_INDEX_PG_URL; use config set index-pg-url --stdin".to_owned()
                })?;
                // The URL itself is never echoed in the error.
                let pg = PostgresIssueIndex::open(&url).await?;
                Ok(Self::Postgres(pg))
            }
        }
    }

    /// Synchronous wrapper for the CLI's `run` entry point, which is
    /// currently synchronous. Creates a dedicated current-thread Tokio
    /// runtime and blocks on the async open. Must not be called from
    /// within an existing Tokio runtime; that path would deadlock with a
    /// `!Send` future and is avoided by keeping automatic search callers
    /// sync and confined to this bridge.
    #[allow(dead_code)]
    pub fn open_blocking() -> Result<Self, String> {
        block_on(Self::open())
    }

    /// Synchronous testable opener.
    #[allow(dead_code)]
    pub fn open_blocking_with_storage(storage: &Storage) -> Result<Self, String> {
        block_on(Self::open_with_storage(storage))
    }
}

#[async_trait(?Send)]
impl IssueIndexStore for IssueIndexBackend {
    async fn upsert(
        &self,
        doc: &crate::providers::index::IssueIndexDocument,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(inner) => inner.upsert(doc).await,
            Self::Postgres(inner) => inner.upsert(doc).await,
        }
    }

    async fn lexical_search(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        include_body: bool,
    ) -> Result<crate::providers::index_store::IssueIndexSearchResult, String> {
        match self {
            Self::Sqlite(inner) => {
                inner
                    .lexical_search(query, limit, offset, include_body)
                    .await
            }
            Self::Postgres(inner) => {
                inner
                    .lexical_search(query, limit, offset, include_body)
                    .await
            }
        }
    }

    async fn lexical_search_scoped(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        include_body: bool,
        scope: &crate::providers::index::LexicalScope,
    ) -> Result<crate::providers::index_store::IssueIndexSearchResult, String> {
        match self {
            Self::Sqlite(inner) => {
                inner
                    .lexical_search_scoped(query, limit, offset, include_body, scope)
                    .await
            }
            Self::Postgres(inner) => {
                inner
                    .lexical_search_scoped(query, limit, offset, include_body, scope)
                    .await
            }
        }
    }
}

/// Resolve the index backend kind from the PostgreSQL URL presence.
/// A non-empty `PHASEGENT_INDEX_PG_URL` from the environment (highest
/// precedence) or persisted global setting selects PostgreSQL; an absent
/// or blank URL selects SQLite. The legacy `PHASEGENT_INDEX_BACKEND`
/// environment variable and persisted setting are intentionally ignored
/// so they can never force a different backend or make selection
/// ambiguous; they remain readable/clearable only for compatibility.
pub fn resolve_index_backend(storage: &Storage) -> Result<IndexBackendKind, String> {
    let url = resolve_pg_url(storage)?;
    match url {
        Some(value) if !value.trim().is_empty() => Ok(IndexBackendKind::Postgres),
        _ => Ok(IndexBackendKind::Sqlite),
    }
}

/// Resolve the PostgreSQL URL via env override then persistent storage.
/// Returns `None` when unset or blank; a blank environment value is
/// treated as unset so resolution falls back to the persisted setting,
/// matching the local credentials/config boundary. Persisted blanks are
/// already normalised to `None` by `Storage::load_global_setting`.
/// The caller decides whether `None` is an error (postgres backend
/// requires it) or acceptable (sqlite backend).
pub fn resolve_pg_url(storage: &Storage) -> Result<Option<String>, String> {
    if let Some(env) = read_env_trimmed("PHASEGENT_INDEX_PG_URL")? {
        if env.trim().is_empty() {
            return Ok(None);
        }
        return Ok(Some(env));
    }
    Ok(storage.load_global_setting("PHASEGENT_INDEX_PG_URL")?)
}

/// Helper used by `resolve_index_backend` / `resolve_pg_url` so every
/// `VarError` other than `NotPresent` surfaces instead of being silently
/// swallowed by the SQLite fallback.
fn read_env_trimmed(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) => {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed))
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("could not read {name}: {error}")),
    }
}

/// Narrow `block_on` bridge for the CLI's sync entry point. Creates a
/// current-thread Tokio runtime and blocks on the future, which may be
/// `!Send` (SQLite). Must not be called from within an async context;
/// the CLI is sync so this is safe. A separate `open` async path exists
/// for tests that already run inside Tokio.
pub fn block_on<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    // Fast path: not inside a Tokio runtime -> create a fresh current_thread runtime.
    // The runtime's `block_on` does not require `Send` and can drive `!Send` futures.
    // If we are already inside a runtime, creating another runtime and blocking
    // would deadlock a `!Send` future (cannot be sent to another thread). The
    // CLI never calls this from within a runtime; tests that need async use the
    // `open(...).await` path directly.
    if tokio::runtime::Handle::try_current().is_ok() {
        // We are inside a runtime; avoid silent deadlock by panicking with a
        // clear message rather than blocking the runtime's worker thread.
        // This branch is not exercised by the CLI but keeps the invariant
        // visible for future callers.
        panic!("block_on called from within a Tokio runtime; use async open().await instead");
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("could not build tokio runtime for index")
        .block_on(future)
}
