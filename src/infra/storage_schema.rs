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
/// The literal is duplicated here so the storage layer never depends on
/// `provider_config` while still holding the same string the resolver
/// understands via `FromStr`.
pub(crate) const PROVIDER_GITLAB: &str = "gitlab";
/// Local provider. Same duplication rationale as above: the literal stays
/// in sync with `ProviderKind::Local::as_str` while keeping this layer
/// transport-agnostic. Persists only the provider preference; credential
/// and backend tables are separate.
pub(crate) const PROVIDER_LOCAL: &str = "local";

/// Schema for the phasegent SQLite database.
///
/// The schema is intentionally split across five small tables:
///
/// * `role_config` stores the per-role provider preference plus the
///   Forgejo `api_base` and `repository` fields.
/// * `role_redmine_config` stores the Redmine-only fields so loading a
///   Redmine config never has to guess whether a missing `project_id`
///   belongs to the legacy Forgejo row or to Redmine. The `project_id`
///   column is legacy: new code never reads or writes it and the
///   `Storage::open` migration clears any legacy values, but the column
///   remains for non-destructive compatibility with old databases.
/// * `role_credential` stores per-(role, provider) credentials; the
///   composite primary key lets the same role keep both a Forgejo token
///   and a Redmine API key without collision.
/// * `role_redmine_user` stores the admin-provisioned Redmine identity
///   (`user_id`, `login`) for each agent role. Written when a
///   deterministic service user is found or created via the admin REST
///   API and read on reruns so provisioning is idempotent without
///   re-listing users. Legacy databases gain the table via
///   `CREATE TABLE IF NOT EXISTS` with no destructive migration; legacy
///   `role_credential` rows without a mapping are reconciled by looking
///   up the deterministic login before creating.
/// * `global_setting` stores deployment-level secrets that are not
///   tied to a role (for example the Redmine git mirror plugin key and
///   its repository URL override). `config show` returns their
///   presence and length; the resolver layer reads the value out of
///   SQLite only when the matching environment variable is unset.
/// * `role_gitlab_config` mirrors the Redmine split; its `project_id`
///   column is also legacy for the same reasons.
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

-- The table mirrors the Redmine-only split so GitLab credentials and
-- configuration never collide with the existing Forgejo/Redmine rows, and
-- the resolver can distinguish a GitLab row from a missing row without
-- inspecting either legacy table. The `project_id` column is INTEGER
-- because GitLab identifiers are numeric project ids, unlike Redmine's
-- free-text identifier slug. `project_id` is legacy in both
-- role_redmine_config and role_gitlab_config: new code never reads or
-- writes the column and Storage::open clears legacy values, but the
-- column remains for non-destructive compatibility.
CREATE TABLE IF NOT EXISTS role_gitlab_config (
    role TEXT PRIMARY KEY,
    api_base TEXT,
    project_id INTEGER
);

-- `fingerprint` (last 4 characters, NULL for short secrets) and
-- `credential_updated_at` (epoch seconds) let `config show` and `doctor`
-- identify *which* credential is stored without ever loading the secret
-- for display. Both columns are maintained by `save_credential` and
-- backfilled on first read by `credential_summary`, so pre-existing rows
-- gain them lazily with no destructive migration.
CREATE TABLE IF NOT EXISTS role_credential (
    role TEXT NOT NULL,
    provider TEXT NOT NULL,
    credential TEXT NOT NULL,
    fingerprint TEXT,
    credential_updated_at INTEGER,
    PRIMARY KEY (role, provider)
);

-- One row per agent role holding the Redmine user provisioned through the
-- administrator REST API. Additive so older databases gain the table on
-- open via `CREATE TABLE IF NOT EXISTS` with no data migration.
CREATE TABLE IF NOT EXISTS role_redmine_user (
    role TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL CHECK (user_id > 0),
    login TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS global_setting (
    name TEXT PRIMARY KEY,
    value TEXT
);

-- Execution ledger. The table is additive so databases created by earlier
-- versions remain readable and never need a destructive migration. Rounded
-- hours are retained for the Redmine projection, while elapsed_seconds
-- remains the authoritative exact-duration value.
--
-- Nullable `owner_session_id` / `owner_call_id` columns let the OpenCode
-- plugin record which subagent invocation owns a run without growing the
-- primary key. Existing rows keep their NULL owner; the additive MIGRATIONS
-- block below adds the columns on databases that were initialised before
-- the field existed.
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

-- Agent notification outbox. One row per structured intent, written
-- before delivery so a crash between intent and delivery stays
-- observable. `event` is one of completion, blocked, failure,
-- interruption_suspected, publish_failed. `channel` is the resolved
-- ntfy/webhook/dingtalk/email literal. `title`/`body` are the bounded
-- summaries actually delivered. `status` is pending, delivered, or
-- failed; `error` carries the bounded delivery failure when failed.
CREATE TABLE IF NOT EXISTS notification_deliveries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,
    event TEXT NOT NULL,
    channel TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    issue_id INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    error TEXT
);

CREATE INDEX IF NOT EXISTS notification_deliveries_created_idx
    ON notification_deliveries (created_at DESC);
";

/// Additive migrations applied on every `Storage::open`. Each entry is a
/// `(table, column)` pair the migration runner inspects via
/// `PRAGMA table_info(<table>)` so the step is idempotent across opens;
/// `ALTER TABLE ADD COLUMN` would otherwise error on a database that was
/// initialised with the new schema already present.
pub(crate) const MIGRATIONS: &[(&str, &str, &str)] = &[
    ("role_credential", "fingerprint", "TEXT"),
    ("role_credential", "credential_updated_at", "INTEGER"),
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

/// Worktree auto-isolation switch. When `true`, `worktree acquire` may
/// create an isolated branch/worktree on a conflict; when `false` (the
/// default) acquire reuses the current checkout and only warns. The
/// string doubles as the environment variable name so `config set`
/// persists it without a translation table. Non-secret; resolved
/// env-over-SQLite like every other global.
pub(crate) const GLOBAL_WORKTREE_AUTO: &str = "PHASEGENT_WORKTREE_AUTO";

/// Agent notification channel configuration. One `global_setting` row
/// per field so operators persist each value with `config set` and the
/// resolver keeps the env-over-SQLite precedence used by every other
/// global. Secrets (`*_TOKEN`, `*_SECRET`, `*_PASSWORD`) are write-only:
/// `config show` reports presence/length only and errors never echo
/// values. Non-secret URLs are sanitised before rendering. TOML
/// overlay does not cover notify settings; precedence is env, then
/// SQLite. Default notifier features are ntfy plus webhook;
/// dingtalk/email fields are accepted always but delivery requires the
/// matching `notify-dingtalk` / `notify-email` Cargo feature.
pub(crate) const GLOBAL_NOTIFY_ENABLED: &str = "PHASEGENT_NOTIFY_ENABLED";
pub(crate) const GLOBAL_NOTIFY_CHANNEL: &str = "PHASEGENT_NOTIFY_CHANNEL";
pub(crate) const GLOBAL_NOTIFY_NTFY_BASE_URL: &str = "PHASEGENT_NOTIFY_NTFY_BASE_URL";
pub(crate) const GLOBAL_NOTIFY_NTFY_TOPIC: &str = "PHASEGENT_NOTIFY_NTFY_TOPIC";
pub(crate) const GLOBAL_NOTIFY_NTFY_TOKEN: &str = "PHASEGENT_NOTIFY_NTFY_TOKEN";
pub(crate) const GLOBAL_NOTIFY_WEBHOOK_URL: &str = "PHASEGENT_NOTIFY_WEBHOOK_URL";
pub(crate) const GLOBAL_NOTIFY_WEBHOOK_TOKEN: &str = "PHASEGENT_NOTIFY_WEBHOOK_TOKEN";
pub(crate) const GLOBAL_NOTIFY_DINGTALK_WEBHOOK_URL: &str = "PHASEGENT_NOTIFY_DINGTALK_WEBHOOK_URL";
pub(crate) const GLOBAL_NOTIFY_DINGTALK_SECRET: &str = "PHASEGENT_NOTIFY_DINGTALK_SECRET";
pub(crate) const GLOBAL_NOTIFY_DINGTALK_KEYWORDS: &str = "PHASEGENT_NOTIFY_DINGTALK_KEYWORDS";
pub(crate) const GLOBAL_NOTIFY_DINGTALK_MSG_TYPE: &str = "PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE";
pub(crate) const GLOBAL_NOTIFY_EMAIL_SMTP_HOST: &str = "PHASEGENT_NOTIFY_EMAIL_SMTP_HOST";
pub(crate) const GLOBAL_NOTIFY_EMAIL_SMTP_PORT: &str = "PHASEGENT_NOTIFY_EMAIL_SMTP_PORT";
pub(crate) const GLOBAL_NOTIFY_EMAIL_TLS: &str = "PHASEGENT_NOTIFY_EMAIL_TLS";
pub(crate) const GLOBAL_NOTIFY_EMAIL_USERNAME: &str = "PHASEGENT_NOTIFY_EMAIL_USERNAME";
pub(crate) const GLOBAL_NOTIFY_EMAIL_PASSWORD: &str = "PHASEGENT_NOTIFY_EMAIL_PASSWORD";
pub(crate) const GLOBAL_NOTIFY_EMAIL_FROM: &str = "PHASEGENT_NOTIFY_EMAIL_FROM";
pub(crate) const GLOBAL_NOTIFY_EMAIL_TO: &str = "PHASEGENT_NOTIFY_EMAIL_TO";
pub(crate) const GLOBAL_NOTIFY_EMAIL_REPLY_TO: &str = "PHASEGENT_NOTIFY_EMAIL_REPLY_TO";

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
    GLOBAL_WORKTREE_AUTO,
    GLOBAL_NOTIFY_ENABLED,
    GLOBAL_NOTIFY_CHANNEL,
    GLOBAL_NOTIFY_NTFY_BASE_URL,
    GLOBAL_NOTIFY_NTFY_TOPIC,
    GLOBAL_NOTIFY_NTFY_TOKEN,
    GLOBAL_NOTIFY_WEBHOOK_URL,
    GLOBAL_NOTIFY_WEBHOOK_TOKEN,
    GLOBAL_NOTIFY_DINGTALK_WEBHOOK_URL,
    GLOBAL_NOTIFY_DINGTALK_SECRET,
    GLOBAL_NOTIFY_DINGTALK_KEYWORDS,
    GLOBAL_NOTIFY_DINGTALK_MSG_TYPE,
    GLOBAL_NOTIFY_EMAIL_SMTP_HOST,
    GLOBAL_NOTIFY_EMAIL_SMTP_PORT,
    GLOBAL_NOTIFY_EMAIL_TLS,
    GLOBAL_NOTIFY_EMAIL_USERNAME,
    GLOBAL_NOTIFY_EMAIL_PASSWORD,
    GLOBAL_NOTIFY_EMAIL_FROM,
    GLOBAL_NOTIFY_EMAIL_TO,
    GLOBAL_NOTIFY_EMAIL_REPLY_TO,
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
