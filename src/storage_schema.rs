//! Schema and PRAGMA constants for the phasegent SQLite database.
//!
//! Kept as separate top-level `const`s so reviewers can read the data
//! model at a glance instead of scanning implementation code. The
//! schema lives in plain SQL strings rather than `.sql` files because
//! `rusqlite` (the only Rust SQLite dependency in the workspace) does
//! not support compile-time-checked query macros, and a few short
//! statements do not justify the indirection of file-based loading.

/// Filename of the SQLite database inside the phasegent config directory.
pub(crate) const DB_FILENAME: &str = "phasegent.sqlite3";

/// Role provider kinds we persist. Mirrors `provider_config::ProviderKind`
/// without pulling in that module to keep this layer transport-agnostic.
pub(crate) const PROVIDER_FORGEJO: &str = "forgejo";
pub(crate) const PROVIDER_REDMINE: &str = "redmine";
/// GitLab is a Foundation-phase addition. The literal is duplicated here
/// so the storage layer never depends on `provider_config` while still
/// holding the same string the resolver understands via `FromStr`.
pub(crate) const PROVIDER_GITLAB: &str = "gitlab";

/// Schema for the phasegent SQLite database.
///
/// The schema is intentionally split across five small tables:
///
/// * `role_config` stores the per-role provider preference plus the
///   Forgejo `api_base` and `repository` fields.
/// * `role_redmine_config` stores the Redmine-only fields so loading a
///   Redmine config never has to guess whether a missing `project_id`
///   belongs to the legacy Forgejo row or to Redmine.
/// * `role_credential` stores per-(role, provider) credentials; the
///   composite primary key lets the same role keep both a Forgejo token
///   and a Redmine API key without collision.
/// * `global_setting` stores deployment-level secrets that are not
///   tied to a role (for example the Redmine git mirror plugin key and
///   its repository URL override). `config show` returns their
///   presence and length; the resolver layer reads the value out of
///   SQLite only when the matching environment variable is unset.
///
/// All non-key columns are nullable so the layer can distinguish
/// "missing" (no row) from "present but empty" (row with NULL).
pub(crate) const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS role_config (
    role TEXT PRIMARY KEY,
    provider TEXT,
    api_base TEXT,
    repository TEXT
);

CREATE TABLE IF NOT EXISTS role_redmine_config (
    role TEXT PRIMARY KEY,
    api_base TEXT,
    project_id TEXT,
    close_status_id INTEGER
);

-- Phase 1 GitLab foundation. The table mirrors the Redmine-only split so
-- GitLab credentials and configuration never collide with the existing
-- Forgejo/Redmine rows, and the resolver can distinguish a GitLab row
-- from a missing row without inspecting either legacy table. The
-- `project_id` column is INTEGER because GitLab identifiers are numeric
-- project ids, unlike Redmine's free-text identifier slug.
CREATE TABLE IF NOT EXISTS role_gitlab_config (
    role TEXT PRIMARY KEY,
    api_base TEXT,
    project_id INTEGER
);

CREATE TABLE IF NOT EXISTS role_credential (
    role TEXT NOT NULL,
    provider TEXT NOT NULL,
    credential TEXT NOT NULL,
    PRIMARY KEY (role, provider)
);

CREATE TABLE IF NOT EXISTS global_setting (
    name TEXT PRIMARY KEY,
    value TEXT
);

-- Phase 5A execution ledger.  The table is additive so databases created
-- by earlier phasegent versions remain readable and never need a destructive
-- migration.  Rounded hours are retained for the Redmine projection, while
-- elapsed_seconds remains the authoritative exact-duration value.
CREATE TABLE IF NOT EXISTS execution_timer_runs (
    run_id TEXT PRIMARY KEY,
    issue_id INTEGER NOT NULL CHECK (issue_id > 0),
    phase TEXT NOT NULL,
    role TEXT NOT NULL,
    attempt INTEGER NOT NULL CHECK (attempt > 0),
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    status TEXT NOT NULL,
    elapsed_seconds INTEGER,
    rounded_hours REAL,
    activity_id INTEGER,
    redmine_time_entry_id INTEGER,
    sync_status TEXT NOT NULL DEFAULT 'pending',
    sync_error TEXT
);

CREATE INDEX IF NOT EXISTS execution_timer_runs_issue_phase_idx
    ON execution_timer_runs (issue_id, phase, role, attempt);
";

/// Names of the deployment-level settings stored in `global_setting`.
/// The strings double as the canonical environment variable names so
/// `config import-env` can persist them without a translation table.
pub(crate) const GLOBAL_REDMINE_GIT_MIRROR_API_KEY: &str = "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY";
pub(crate) const GLOBAL_REDMINE_REPOSITORY_URL: &str = "PHASEGENT_REDMINE_REPOSITORY_URL";
/// Persistent machine-wide default provider. Acts as the fallback
/// between `PHASEGENT_PROVIDER` (one-process override) and the
/// role-scoped `role_config.provider` so operators can switch between
/// an external Redmine deployment and an internal GitLab one without
/// touching every role-scoped config row. The string doubles as the
/// environment variable name so `config import-env` persists it
/// without a translation table.
pub(crate) const GLOBAL_DEFAULT_PROVIDER: &str = "PHASEGENT_DEFAULT_PROVIDER";

/// All `global_setting` row names the resolver layer currently
/// recognises. Listed in one place so `config show` can iterate over
/// the canonical set without relying on string constants scattered
/// across modules.
pub(crate) const GLOBAL_SETTING_NAMES: &[&str] = &[
    GLOBAL_REDMINE_GIT_MIRROR_API_KEY,
    GLOBAL_REDMINE_REPOSITORY_URL,
    GLOBAL_DEFAULT_PROVIDER,
];

/// Statement used by the schema initializer. Splitting `PRAGMA`s from
/// the table DDL keeps WAL toggles and busy-timeouts inspectable next
/// to the table layout.
pub(crate) const PRAGMA_STATEMENTS: &str = "\
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
";
