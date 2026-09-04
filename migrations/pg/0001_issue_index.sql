-- PostgreSQL issue index schema.
-- Mirrors the SQLite issue_documents / issue_chunks contract but uses
-- native PostgreSQL types, a tsvector column, and a GIN index.
-- Only index tables live here; credentials and timer state remain in
-- the SQLite database (phasegent.sqlite3).
-- The migration is embedded via include_str! and auto-applied with
-- version tracking (_issue_index_migrations) when the postgres backend opens.

CREATE TABLE IF NOT EXISTS issue_documents (
    source TEXT NOT NULL CHECK (source <> '' AND length(source) <= 64),
    project TEXT NOT NULL CHECK (project <> '' AND length(project) <= 200),
    external_id TEXT NOT NULL CHECK (external_id <> '' AND length(external_id) <= 128),
    issue_number BIGINT NOT NULL CHECK (issue_number >= 0),
    title TEXT NOT NULL CHECK (title <> '' AND length(title) <= 1024 AND length(title) <= 8192),
    body TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state <> '' AND length(state) <= 64),
    url TEXT CHECK (url IS NULL OR length(url) <= 2048),
    provider_updated_at BIGINT CHECK (provider_updated_at IS NULL OR provider_updated_at > 0),
    indexed_at BIGINT NOT NULL CHECK (indexed_at > 0),
    content_hash TEXT NOT NULL CHECK (content_hash <> ''),
    deleted BOOLEAN NOT NULL DEFAULT FALSE,
    deleted_at BIGINT CHECK (deleted_at IS NULL OR deleted_at > 0),
    search_vector tsvector,
    PRIMARY KEY (source, project, external_id),
    CHECK ((deleted = FALSE AND deleted_at IS NULL) OR (deleted = TRUE AND deleted_at IS NOT NULL)),
    CHECK ((deleted = TRUE AND issue_number = 0) OR (deleted = FALSE AND issue_number > 0))
);

CREATE TABLE IF NOT EXISTS issue_chunks (
    source TEXT NOT NULL,
    project TEXT NOT NULL,
    external_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    text TEXT NOT NULL,
    byte_start INTEGER NOT NULL CHECK (byte_start >= 0),
    byte_end INTEGER NOT NULL CHECK (byte_end >= byte_start),
    hash TEXT NOT NULL CHECK (hash <> ''),
    PRIMARY KEY (source, project, external_id, ordinal),
    FOREIGN KEY (source, project, external_id) REFERENCES issue_documents(source, project, external_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS issue_chunks_doc_idx ON issue_chunks (source, project, external_id);

CREATE INDEX IF NOT EXISTS issue_documents_search_vector_idx ON issue_documents USING GIN (search_vector);

-- Trigger to keep search_vector in sync with title/body.
CREATE OR REPLACE FUNCTION issue_documents_search_vector_update() RETURNS trigger AS $$
BEGIN
    NEW.search_vector := to_tsvector('english', coalesce(NEW.title, '') || ' ' || coalesce(NEW.body, ''));
    RETURN NEW;
END
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS issue_documents_search_vector_trigger ON issue_documents;
CREATE TRIGGER issue_documents_search_vector_trigger
    BEFORE INSERT OR UPDATE OF title, body ON issue_documents
    FOR EACH ROW EXECUTE FUNCTION issue_documents_search_vector_update();
