-- PostgreSQL local provider schema (issue 211 P4, migration 0002).
-- Mirrors src/infra/local_sql/schema.sql + seed.sql with identical column
-- names; only types adapt to PostgreSQL (BIGSERIAL/BIGINT, BOOLEAN-free).
-- Only local tables live here; credentials/timers stay in phasegent.sqlite3
-- and the index stays in 0001 tables. Additive only, version-tracked by
-- _local_migrations (version 2) via apply_embedded_migrations in
-- src/infra/local_store.rs. Single-active backend: selected by the same
-- PHASEGENT_INDEX_PG_URL as the index, never dual-written with SQLite.

CREATE TABLE IF NOT EXISTS local_projects (
    project TEXT PRIMARY KEY CHECK (project <> '' AND length(project) <= 200),
    description TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK (created_at > 0)
);

CREATE TABLE IF NOT EXISTS local_issues (
    id BIGSERIAL PRIMARY KEY,
    title TEXT NOT NULL CHECK (title <> '' AND length(title) <= 1024),
    body TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK (status <> '' AND length(status) <= 64),
    project TEXT NOT NULL DEFAULT 'default',
    tracker TEXT NOT NULL DEFAULT 'Task',
    author_role TEXT NOT NULL DEFAULT 'executor',
    created_at BIGINT NOT NULL CHECK (created_at > 0),
    updated_at BIGINT NOT NULL CHECK (updated_at > 0),
    closed_at BIGINT CHECK (closed_at IS NULL OR closed_at > 0)
);

CREATE INDEX IF NOT EXISTS local_issues_project_status_idx
    ON local_issues (project, status);

CREATE TABLE IF NOT EXISTS local_comments (
    id BIGSERIAL PRIMARY KEY,
    issue_id BIGINT NOT NULL REFERENCES local_issues(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role <> ''),
    phase TEXT NOT NULL DEFAULT '',
    attempt INTEGER NOT NULL DEFAULT 1 CHECK (attempt > 0),
    marker TEXT NOT NULL UNIQUE CHECK (marker <> ''),
    body TEXT NOT NULL,
    created_at BIGINT NOT NULL CHECK (created_at > 0)
);

CREATE INDEX IF NOT EXISTS local_comments_issue_idx
    ON local_comments (issue_id);

CREATE TABLE IF NOT EXISTS status_transitions (
    from_status TEXT NOT NULL,
    to_status TEXT NOT NULL,
    PRIMARY KEY (from_status, to_status)
);

-- Seeds mirror local_sql/seed.sql; ON CONFLICT DO NOTHING keeps re-apply safe.
INSERT INTO local_projects (project, description, created_at)
    VALUES ('default', 'Default local project', 1700000000)
    ON CONFLICT (project) DO NOTHING;

INSERT INTO status_transitions (from_status, to_status) VALUES
    ('New', 'In Progress'),
    ('New', 'Cancelled'),
    ('In Progress', 'In Review'),
    ('In Progress', 'Blocked'),
    ('In Progress', 'Cancelled'),
    ('In Review', 'Resolved'),
    ('In Review', 'Changes Requested'),
    ('In Review', 'Blocked'),
    ('In Review', 'Cancelled'),
    ('Changes Requested', 'In Progress'),
    ('Changes Requested', 'Blocked'),
    ('Changes Requested', 'Cancelled'),
    ('Blocked', 'In Progress'),
    ('Blocked', 'Cancelled'),
    ('Resolved', 'In Progress'),
    ('Resolved', 'Closed')
    ON CONFLICT DO NOTHING;
