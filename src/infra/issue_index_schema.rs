//! Schema and PRAGMA constants for the independent issue index SQLite.
//!
//! The index lives in a separate file from the configuration and
//! credentials database so local credentials never migrate to a shared
//! backend. The schema is kept in constants for reviewability; callers
//! via `SqliteIssueIndex::open` handle the file lifecycle.

/// Filename of the private index SQLite inside the phasegent config
/// directory. The separate suffix guarantees the file never collides
/// with the operator's `phasegent.sqlite3` credentials store.
pub(crate) const DB_FILENAME_INDEX: &str = "phasegent-index.sqlite3";

/// PRAGMAs for the index connection. Mirrors the main database
/// settings and keeps the index usable under concurrent readers.
pub(crate) const PRAGMA_STATEMENTS_INDEX: &str = "\
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
";

/// Schema for the local issue index.
///
/// Two tables provide the whole surface so far:
///
/// * `issue_documents` is the indexed copy of an external issue,
///   keyed by the stable `(source, project, external_id)` triple that
///   `IssueIndexKey` establishes. `deleted` marks a tombstone;
///   `deleted_at` is the wall-clock that created it.
/// * `issue_chunks` holds the bounded, UTF-8-safe slices of `body`.
///   The foreign key is `ON DELETE CASCADE` so a tombstone that
///   deletes the document also removes its chunks, while an upsert
///   replaces the chunk set atomically inside the same transaction.
///
/// No other tables exist yet; a later Postgres backend will reuse the
/// same column set and the provider-neutral trait.
pub(crate) const SCHEMA_INDEX: &str = "\
CREATE TABLE IF NOT EXISTS issue_documents (
    source TEXT NOT NULL,
    project TEXT NOT NULL,
    external_id TEXT NOT NULL,
    issue_number INTEGER NOT NULL CHECK (issue_number >= 0),
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    state TEXT NOT NULL,
    url TEXT,
    provider_updated_at INTEGER CHECK (provider_updated_at IS NULL OR provider_updated_at > 0),
    indexed_at INTEGER NOT NULL CHECK (indexed_at > 0),
    content_hash TEXT NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    deleted_at INTEGER CHECK (deleted_at IS NULL OR deleted_at > 0),
    PRIMARY KEY (source, project, external_id)
);

CREATE TABLE IF NOT EXISTS issue_chunks (
    source TEXT NOT NULL,
    project TEXT NOT NULL,
    external_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    text TEXT NOT NULL,
    byte_start INTEGER NOT NULL CHECK (byte_start >= 0),
    byte_end INTEGER NOT NULL CHECK (byte_end >= byte_start),
    hash TEXT NOT NULL,
    PRIMARY KEY (source, project, external_id, ordinal),
    FOREIGN KEY (source, project, external_id)
        REFERENCES issue_documents(source, project, external_id)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS issue_chunks_doc_idx
    ON issue_chunks (source, project, external_id);
";
