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
///   belongs to the legacy Forgejo row or to Redmine. The `project_id`
///   column is legacy in Phase 1 (remove-project-id); new code never
///   reads or writes it and the `Storage::open` migration clears any
///   legacy values, but the column remains for non-destructive
///   compatibility with old databases.
/// * `role_credential` stores per-(role, provider) credentials; the
///   composite primary key lets the same role keep both a Forgejo token
///   and a Redmine API key without collision.
/// * `global_setting` stores deployment-level secrets that are not
///   tied to a role (for example the Redmine git mirror plugin key and
///   its repository URL override). `config show` returns their
///   presence and length; the resolver layer reads the value out of
///   SQLite only when the matching environment variable is unset.
/// * `role_gitlab_config` mirrors the Redmine split; its `project_id`
///   column is also legacy in Phase 1 for the same reasons.
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
-- Phase 1 (remove-project-id) makes `project_id` legacy in both
-- role_redmine_config and role_gitlab_config: new code never reads or
-- writes the column and Storage::open clears legacy values, but the
-- column remains for non-destructive compatibility.
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
--
-- Phase 3 adds nullable `owner_session_id` / `owner_call_id` columns so the
-- OpenCode plugin can record which subagent invocation owns a run without
-- growing the primary key. Existing rows keep their NULL owner; the
-- additive MIGRATIONS block below adds the columns on databases that were
-- initialised before the field existed.
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
    sync_error TEXT,
    owner_session_id TEXT,
    owner_call_id TEXT,
    projection_token TEXT,
    projection_claimed_at INTEGER
);

CREATE INDEX IF NOT EXISTS execution_timer_runs_issue_phase_idx
    ON execution_timer_runs (issue_id, phase, role, attempt);

CREATE INDEX IF NOT EXISTS execution_timer_runs_status_idx
    ON execution_timer_runs (status, started_at DESC);
";

/// Additive migrations applied on every `Storage::open`. Each entry is a
/// `(table, column)` pair the migration runner inspects via
/// `PRAGMA table_info(<table>)` so the step is idempotent across opens;
/// `ALTER TABLE ADD COLUMN` would otherwise error on a database that was
/// initialised with the new schema already present.
pub(crate) const MIGRATIONS: &[(&str, &str, &str)] = &[
    ("execution_timer_runs", "owner_session_id", "TEXT"),
    ("execution_timer_runs", "owner_call_id", "TEXT"),
    ("execution_timer_runs", "projection_token", "TEXT"),
    ("execution_timer_runs", "projection_claimed_at", "INTEGER"),
];

/// Names of the deployment-level settings stored in `global_setting`.
/// The strings double as the canonical environment variable names so
/// `config set` can persist them without a translation table.
pub(crate) const GLOBAL_REDMINE_GIT_MIRROR_API_KEY: &str = "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY";
pub(crate) const GLOBAL_REDMINE_REPOSITORY_URL: &str = "PHASEGENT_REDMINE_REPOSITORY_URL";
/// Persistent machine-wide default provider. Acts as the fallback
/// between `PHASEGENT_PROVIDER` (one-process override) and the
/// role-scoped `role_config.provider` so operators can switch between
/// an external Redmine deployment and an internal GitLab one without
/// touching every role-scoped config row. The string doubles as the
/// environment variable name so `config set` persists it
/// without a translation table.
pub(crate) const GLOBAL_DEFAULT_PROVIDER: &str = "PHASEGENT_DEFAULT_PROVIDER";
/// Issue index backend selector. When `postgres` the index lives in
/// PostgreSQL (shared, multi-machine); default `sqlite` keeps the
/// local `phasegent-index.sqlite3` file. The string doubles as the
/// environment variable name.
pub(crate) const GLOBAL_INDEX_BACKEND: &str = "PHASEGENT_INDEX_BACKEND";
/// PostgreSQL connection URL for the shared issue index. Stored as a
/// secret global setting and never echoed in snapshots or errors.
pub(crate) const GLOBAL_INDEX_PG_URL: &str = "PHASEGENT_INDEX_PG_URL";

/// All `global_setting` row names the resolver layer currently
/// recognises. Listed in one place so `config show` can iterate over
/// the canonical set without relying on string constants scattered
/// across modules.
pub(crate) const GLOBAL_SETTING_NAMES: &[&str] = &[
    GLOBAL_REDMINE_GIT_MIRROR_API_KEY,
    GLOBAL_REDMINE_REPOSITORY_URL,
    GLOBAL_DEFAULT_PROVIDER,
    GLOBAL_INDEX_BACKEND,
    GLOBAL_INDEX_PG_URL,
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
