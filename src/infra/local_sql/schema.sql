-- Local provider SQLite schema (issue 211 P4).
-- Independent file phasegent-local.sqlite3; never touches
-- phasegent.sqlite3 (config/credentials) or phasegent-index.sqlite3.
-- Additive only: CREATE TABLE/INDEX IF NOT EXISTS, no destructive steps.
-- Mirrored by migrations/pg/0002_local.sql with identical column names.

CREATE TABLE IF NOT EXISTS local_projects (
    project TEXT PRIMARY KEY CHECK (project <> '' AND length(project) <= 200),
    description TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL CHECK (created_at > 0)
);

CREATE TABLE IF NOT EXISTS local_issues (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL CHECK (title <> '' AND length(title) <= 1024),
    body TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK (status <> '' AND length(status) <= 64),
    project TEXT NOT NULL DEFAULT 'default',
    tracker TEXT NOT NULL DEFAULT 'Task',
    author_role TEXT NOT NULL DEFAULT 'executor',
    created_at INTEGER NOT NULL CHECK (created_at > 0),
    updated_at INTEGER NOT NULL CHECK (updated_at > 0),
    closed_at INTEGER CHECK (closed_at IS NULL OR closed_at > 0)
);

CREATE INDEX IF NOT EXISTS local_issues_project_status_idx
    ON local_issues (project, status);

CREATE TABLE IF NOT EXISTS local_comments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    issue_id INTEGER NOT NULL REFERENCES local_issues(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role <> ''),
    phase TEXT NOT NULL DEFAULT '',
    attempt INTEGER NOT NULL DEFAULT 1 CHECK (attempt > 0),
    marker TEXT NOT NULL UNIQUE CHECK (marker <> ''),
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL CHECK (created_at > 0)
);

CREATE INDEX IF NOT EXISTS local_comments_issue_idx
    ON local_comments (issue_id);

CREATE TABLE IF NOT EXISTS status_transitions (
    from_status TEXT NOT NULL,
    to_status TEXT NOT NULL,
    PRIMARY KEY (from_status, to_status)
);
