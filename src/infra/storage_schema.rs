//! Schema and PRAGMA constants for the phasegent SQLite database.
//!
//! Kept as separate top-level `const`s so reviewers can read the data
//! model at a glance instead of scanning implementation code. The
//! schema lives in plain SQL strings rather than `.sql` files because
//! `rusqlite` (the only Rust SQLite dependency in the workspace) does
//! not support compile-time-checked query macros, and a few short
//! statements do not justify the indirection of file-based loading.

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
/// The database holds business state only: the execution ledger, the
/// agent notification outbox, and the canonical task identity tables.
/// Configuration and credentials live in the JSONC configuration file
/// (`crate::infra::config_file`), so this schema creates no
/// configuration table and reads no configuration value. Databases
/// created by earlier versions keep their legacy configuration tables;
/// the `admin config migrate` command is the only thing that clears
/// them, and it never drops a table.
///
/// All non-key columns are nullable so the layer can distinguish
/// "missing" (no row) from "present but empty" (row with NULL).
pub(crate) const SCHEMA: &str = "\
-- Execution ledger. The table is additive so databases created by earlier
-- versions remain readable and never need a destructive migration. Rounded
-- hours are retained for the Redmine projection, while elapsed_seconds
-- remains the authoritative exact-duration value.
--
-- Nullable `owner_session_id` / `owner_call_id` columns let the OpenCode
-- plugin record which subagent invocation owns a run without growing the
-- primary key. Existing rows keep their NULL owner; the additive MIGRATIONS
-- block below adds the columns on databases that were initialised before the
-- field existed.
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

-- Durable canonical task identity (issue 1201 P5). One immutable row per
-- provider+instance+project+external_id four-tuple, addressed by an opaque
-- local task_id. Additive so databases created by earlier versions gain the
-- tables on open; nothing is inferred or backfilled from branch, lease or
-- index data.
CREATE TABLE IF NOT EXISTS task_bindings (
    task_id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL CHECK (length(provider) > 0),
    instance TEXT NOT NULL CHECK (length(instance) > 0),
    project TEXT NOT NULL CHECK (length(project) > 0),
    external_id TEXT NOT NULL CHECK (length(external_id) > 0),
    created_at INTEGER NOT NULL CHECK (created_at > 0),
    UNIQUE (provider, instance, project, external_id)
);

CREATE INDEX IF NOT EXISTS task_bindings_external_idx
    ON task_bindings (external_id);

CREATE INDEX IF NOT EXISTS task_bindings_project_idx
    ON task_bindings (provider, instance, project);

-- Explicit physical-repository membership for a task. The repo key is a
-- membership hint, never provider ownership, so a bare number only resolves
-- inside the repositories that were explicitly linked.
CREATE TABLE IF NOT EXISTS task_repo_memberships (
    repo_key TEXT NOT NULL CHECK (length(repo_key) > 0),
    task_id INTEGER NOT NULL REFERENCES task_bindings(task_id) ON DELETE RESTRICT,
    created_at INTEGER NOT NULL CHECK (created_at > 0),
    PRIMARY KEY (repo_key, task_id)
);

CREATE INDEX IF NOT EXISTS task_repo_memberships_task_idx
    ON task_repo_memberships (task_id);
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

/// Names of the deployment-level settings stored in the JSONC
/// configuration file's `settings` map.
/// The strings double as the canonical environment variable names so
/// `config set` can persist them without a translation table.
pub(crate) const GLOBAL_REDMINE_GIT_MIRROR_API_KEY: &str = "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY";
pub(crate) const GLOBAL_REDMINE_REPOSITORY_URL: &str = "PHASEGENT_REDMINE_REPOSITORY_URL";
/// Canonical, non-secret global Redmine REST API base. Every Redmine
/// role/provider request resolves the same address from this one
/// setting (or its environment override) instead of a per-role value.
/// The string doubles as the environment variable name.
pub(crate) const GLOBAL_REDMINE_API_BASE: &str = "PHASEGENT_REDMINE_API_BASE";
/// Persistent machine-wide default provider. Acts as the fallback
/// between `PHASEGENT_PROVIDER` (one-process override) and the
/// role-scoped provider preference so operators can switch between
/// an external Redmine deployment and an internal GitLab one without
/// touching every role-scoped config entry. The string doubles as the
/// environment variable name so `config set` persists it
/// without a translation table.
pub(crate) const GLOBAL_DEFAULT_PROVIDER: &str = "PHASEGENT_DEFAULT_PROVIDER";
/// PostgreSQL connection URL for the shared issue index. Stored as a
/// secret global setting and never echoed in snapshots or errors.
pub(crate) const GLOBAL_INDEX_PG_URL: &str = "PHASEGENT_INDEX_PG_URL";

/// Agent notification channel configuration. One setting per field so
/// operators persist each value with `config set` and the resolver
/// keeps the env-over-file precedence used by every other global.
/// Secrets (`*_TOKEN`, `*_SECRET`, `*_PASSWORD`) are write-only:
/// `config show` reports presence/length only and errors never echo
/// values. Non-secret URLs are sanitised before rendering. Default
/// notifier features are ntfy plus webhook;
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

/// All global setting names the resolver layer currently recognises.
/// Listed in one place so the migration importer can validate a
/// persisted name against the canonical set without relying on string
/// constants scattered across modules. The names double as the
/// environment override names; the configuration file itself stores the
/// typed hierarchy, never these names.
pub(crate) const GLOBAL_SETTING_NAMES: &[&str] = &[
    GLOBAL_REDMINE_GIT_MIRROR_API_KEY,
    GLOBAL_REDMINE_REPOSITORY_URL,
    GLOBAL_REDMINE_API_BASE,
    GLOBAL_DEFAULT_PROVIDER,
    GLOBAL_INDEX_PG_URL,
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
