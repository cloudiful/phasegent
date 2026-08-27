//! SQLite-backed persistence for phasegent role configuration and credentials.
//!
//! The storage module keeps role/provider configuration, per-role
//! credentials, and the small set of machine-wide settings in a single
//! SQLite database. The database lives at the OS-standard config
//! location returned by [`directories::ProjectDirs`]: `~/.config/phasegent`
//! on Linux, `~/Library/Application Support/com.Cloud1ful.phasegent` on
//! macOS, and `%APPDATA%\Cloud1ful\phasegent\config` on Windows. The
//! schema splits role-scoped provider configuration from role/provider
//! credentials so `config show` can mask secrets without leaking their
//! content.
//!
//! The schema lives in [`storage_schema`] so the data model stays
//! readable as a whole; this file stays focused on the public CRUD
//! surface and the platform-aware path resolver.

use crate::auth::{GitlabStoredConfig, RedmineStoredConfig, StoredConfig};
use crate::policy::Role;
use crate::storage_schema::{GLOBAL_SETTING_NAMES, MIGRATIONS, PRAGMA_STATEMENTS, SCHEMA};
use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) use crate::storage_schema::{
    DB_FILENAME, PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE,
};

/// Re-export the canonical global setting names so callers do not
// need to depend on `storage_schema` directly.
pub(crate) use crate::storage_schema::{
    GLOBAL_REDMINE_GIT_MIRROR_API_KEY, GLOBAL_REDMINE_REPOSITORY_URL,
};

/// Metadata about a single `global_setting` row. The full secret
/// value never leaves the storage layer through this struct; only the
/// length is exposed, which keeps `config show` redacted while still
/// surfacing "configured/empty/missing" semantics to operators.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalSettingSummary {
    pub name: &'static str,
    pub present: bool,
    pub length: usize,
}

impl GlobalSettingSummary {
    pub(crate) fn missing(name: &'static str) -> Self {
        Self {
            name,
            present: false,
            length: 0,
        }
    }
}

/// A single wall-clock phase run persisted in the local execution ledger.
///
/// `elapsed_seconds` is always the exact whole-second difference between the
/// persisted timestamps. `rounded_hours` is the value projected to Redmine;
/// the latter is deliberately derived rather than used as the source of
/// truth.
///
/// `owner_session_id` and `owner_call_id` are the optional OpenCode
/// subagent identifiers recorded by the plugin when a run is started. They
/// are never used for idempotency (run_id remains the only marker) and are
/// never surfaced as remote projections.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TimerRun {
    pub run_id: String,
    pub issue: u64,
    pub phase: String,
    pub role: String,
    pub attempt: u64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub elapsed_seconds: Option<i64>,
    pub rounded_hours: Option<f64>,
    pub activity_id: Option<u64>,
    pub time_entry_id: Option<u64>,
    pub sync_status: String,
    pub sync_error: Option<String>,
    pub owner_session_id: Option<String>,
    pub owner_call_id: Option<String>,
    pub projection_token: Option<String>,
    pub projection_claimed_at: Option<i64>,
}

pub(crate) const TIMER_STATUS_RUNNING: &str = "running";
pub(crate) const TIMER_SYNC_PENDING: &str = "pending";
pub(crate) const TIMER_SYNC_SYNCED: &str = "synced";
pub(crate) const TIMER_SYNC_UNCONFIRMED: &str = "unconfirmed";
pub(crate) const TIMER_SYNC_FAILED: &str = "failed";
pub(crate) const TIMER_SYNC_PROJECTING: &str = "projecting";
pub(crate) const PROJECTION_LEASE_SECS: i64 = 120;
pub(crate) const PROJECTION_TOKEN_BOUND: usize = 128;

/// Filter for the `timer list` surface. Mapped to the same string
/// literals the `execution_timer_runs.status` column stores so a
/// caller-supplied value matches the data without translation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerStatusFilter {
    Running,
    Finished,
    All,
}

impl TimerStatusFilter {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "running" => Ok(Self::Running),
            "finished" => Ok(Self::Finished),
            "all" => Ok(Self::All),
            other => Err(format!(
                "timer list --status must be running, finished, or all; got '{other}'"
            )),
        }
    }
}

/// Optional owner metadata recorded by the OpenCode plugin when it starts
/// a run. Both fields are bounded, control-character-free, and treated as
/// run metadata only — they never influence provider projection.
#[derive(Clone, Debug, Default)]
pub struct TimerRunOwner {
    pub session_id: Option<String>,
    pub call_id: Option<String>,
}

/// Result shapes accepted by the local state machine.  Keeping these
/// constants in storage makes it harder for a caller to use an arbitrary
/// remote response as a state transition.
pub(crate) fn valid_timer_sync_status(value: &str) -> bool {
    matches!(
        value,
        TIMER_SYNC_PENDING
            | TIMER_SYNC_SYNCED
            | TIMER_SYNC_UNCONFIRMED
            | TIMER_SYNC_FAILED
            | TIMER_SYNC_PROJECTING
    )
}

/// Handle for the phasegent SQLite database. Cloning is cheap (the
/// underlying connection is reference-counted) so callers can pass it
/// around freely; mutable operations serialise through SQLite's own
/// locking.
pub struct Storage {
    pub(crate) connection: Connection,
    pub(crate) path: PathBuf,
}

impl Storage {
    /// Open the database at the platform-standard config directory
    /// resolved by [`directories::ProjectDirs`]. Returns a structured
    /// error when the host has no usable home / config directory.
    ///
    /// When the `PHASEGENT_DB_PATH` environment variable is set to an
    /// absolute path, that path is used verbatim instead of the
    /// platform-standard config directory. The override exists so
    /// integration tests that drive commands through the CLI layer
    /// can point `Storage::open()` at a temp database without
    /// needing a `--storage-path` flag; production code never sets
    /// this variable.
    pub fn open() -> Result<Self, String> {
        if let Some(override_path) = std::env::var_os("PHASEGENT_DB_PATH") {
            let path = PathBuf::from(override_path);
            return Self::open_at(&path);
        }
        let path = project_dirs_db_path()?;
        Self::open_at(&path)
    }

    /// Open the database at an explicit filesystem path. Used by tests
    /// that want full control over the location. Creates the parent
    /// directory with mode 0700 and the database file with mode 0600
    /// on Unix before handing the connection off.
    pub fn open_at(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let connection = Connection::open(path)
            .map_err(|error| format!("could not open phasegent database: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Make sure the file itself is private even when an
            // existing database already lived on disk with broader
            // permissions.
            let metadata = fs::metadata(path)
                .map_err(|error| format!("could not stat phasegent database: {error}"))?;
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(path, permissions)
                .map_err(|error| format!("could not secure phasegent database: {error}"))?;
        }
        Self::initialise(&connection)?;
        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
    }

    fn initialise(connection: &Connection) -> Result<(), String> {
        connection
            .execute_batch(PRAGMA_STATEMENTS)
            .map_err(|error| format!("could not configure phasegent database: {error}"))?;
        connection
            .execute_batch(SCHEMA)
            .map_err(|error| format!("could not initialise phasegent schema: {error}"))?;
        Self::apply_migrations(connection)?;
        Ok(())
    }

    /// Run every additive `MIGRATIONS` row whose column is not yet present
    /// on `execution_timer_runs`. Idempotent across opens so a database
    /// that already carries the column is untouched. Two processes opening
    /// the same pre-owner database concurrently serialize via an immediate
    /// transaction and tolerate a duplicate-column race as success.
    /// `BEGIN IMMEDIATE` is retried on `busy`/`locked` with bounded
    /// backoff; any lock or commit failure is propagated so callers never
    /// observe a successful open when the schema is not durable.
    fn apply_migrations(connection: &Connection) -> Result<(), String> {
        // Serialize the check+ALTER so two concurrent opens cannot both
        // observe a missing column and then have the loser fail. Retry
        // busy acquisition a few times before surfacing the error.
        let mut begin_attempts = 0;
        loop {
            match connection.execute("BEGIN IMMEDIATE", []) {
                Ok(_) => break,
                Err(error) => {
                    let msg = error.to_string().to_ascii_lowercase();
                    let is_busy = msg.contains("busy") || msg.contains("locked");
                    begin_attempts += 1;
                    if is_busy && begin_attempts < 5 {
                        std::thread::sleep(std::time::Duration::from_millis(
                            10 * begin_attempts as u64,
                        ));
                        continue;
                    }
                    return Err(format!("could not acquire migration lock: {error}"));
                }
            }
        }
        let result = (|| -> Result<(), String> {
            for (table, column, kind) in MIGRATIONS {
                let present = column_exists(connection, table, column)?;
                if present {
                    continue;
                }
                let statement = format!("ALTER TABLE {table} ADD COLUMN {column} {kind}");
                match connection.execute(&statement, []) {
                    Ok(_) => {}
                    Err(error) => {
                        let message = error.to_string();
                        if message.contains("duplicate column name")
                            || message.contains("already exists")
                        {
                            continue;
                        }
                        return Err(format!(
                            "could not migrate column {table}.{column}: {error}"
                        ));
                    }
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                if let Err(error) = connection.execute("COMMIT", []) {
                    let _ = connection.execute("ROLLBACK", []);
                    return Err(format!("could not commit migration: {error}"));
                }
                Ok(())
            }
            Err(error) => {
                let _ = connection.execute("ROLLBACK", []);
                Err(error)
            }
        }
    }

    /// Absolute filesystem path of the database. Useful for `config show`
    /// and tests that want to assert directory layout.
    pub fn db_path(&self) -> &Path {
        &self.path
    }

    /// Load the role-level configuration (provider preference plus the
    /// Forgejo api_base/repository). Returns `None` when no row exists
    /// for `role` so callers can distinguish "never written" from
    /// "written with all fields null".
    pub fn load_role_config(&self, role: Role) -> Result<Option<StoredConfig>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT provider, api_base, repository FROM role_config WHERE role = ?1")
            .map_err(|error| format!("could not prepare role config load: {error}"))?;
        let value = statement
            .query_row(params![role.as_str()], |row| {
                Ok(StoredConfig {
                    provider: row.get(0)?,
                    api_base: row.get(1)?,
                    repository: row.get(2)?,
                })
            })
            .optional()
            .map_err(|error| format!("could not read role config: {error}"))?;
        Ok(value)
    }

    /// Upsert the role-level configuration. `None` fields are stored
    /// as SQL `NULL`; existing non-null values are overwritten.
    pub fn save_role_config(&self, role: Role, config: &StoredConfig) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin role config write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_config (role, provider, api_base, repository) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(role) DO UPDATE SET \
                    provider = excluded.provider, \
                    api_base = excluded.api_base, \
                    repository = excluded.repository",
                params![
                    role.as_str(),
                    config.provider,
                    config.api_base,
                    config.repository,
                ],
            )
            .map_err(|error| format!("could not write role config: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit role config write: {error}"))?;
        Ok(())
    }

    /// Set only the `provider` column on `role_config` without
    /// touching `api_base` or `repository`. Used by `auth setup` when
    /// the caller switches providers without re-supplying the URL.
    pub fn update_provider(&self, role: Role, provider: &str) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin provider update: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_config (role, provider, api_base, repository) \
                 VALUES (?1, ?2, NULL, NULL) \
                 ON CONFLICT(role) DO UPDATE SET provider = excluded.provider",
                params![role.as_str(), provider],
            )
            .map_err(|error| format!("could not update provider: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit provider update: {error}"))?;
        Ok(())
    }

    /// Load the Redmine-specific configuration for `role`. Mirrors the
    /// semantics of [`load_role_config`].
    pub fn load_redmine_config(&self, role: Role) -> Result<Option<RedmineStoredConfig>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT api_base, project_id, close_status_id \
                 FROM role_redmine_config WHERE role = ?1",
            )
            .map_err(|error| format!("could not prepare redmine config load: {error}"))?;
        let value = statement
            .query_row(params![role.as_str()], |row| {
                Ok(RedmineStoredConfig {
                    api_base: row.get(0)?,
                    project_id: row.get(1)?,
                    close_status_id: row.get(2)?,
                    group_name: None,
                    group_role: None,
                })
            })
            .optional()
            .map_err(|error| format!("could not read redmine config: {error}"))?;
        Ok(value)
    }

    /// Upsert the Redmine-specific configuration.
    pub fn save_redmine_config(
        &self,
        role: Role,
        config: &RedmineStoredConfig,
    ) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin redmine config write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_redmine_config (role, api_base, project_id, close_status_id) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(role) DO UPDATE SET \
                    api_base = excluded.api_base, \
                    project_id = excluded.project_id, \
                    close_status_id = excluded.close_status_id",
                params![
                    role.as_str(),
                    config.api_base,
                    config.project_id,
                    config.close_status_id,
                ],
            )
            .map_err(|error| format!("could not write redmine config: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit redmine config write: {error}"))?;
        Ok(())
    }

    /// Set only the bootstrap identity (api_base, project_id,
    /// close_status_id) without disturbing an existing provider
    /// preference on `role_config`. Mirrors the pre-SQLite behaviour
    /// of `auth::persist_redmine_bootstrap`.
    pub fn persist_redmine_bootstrap(
        &self,
        role: Role,
        api_base: Option<String>,
        project_id: u64,
        close_status_id: u64,
    ) -> Result<(), String> {
        if project_id == 0 || close_status_id == 0 {
            return Err(
                "Redmine project and close status IDs must be greater than zero".to_owned(),
            );
        }
        let mut config = self.load_redmine_config(role)?.unwrap_or_default();
        if api_base.is_some() {
            config.api_base = api_base;
        }
        config.project_id = Some(project_id.to_string());
        config.close_status_id = Some(close_status_id);
        self.save_redmine_config(role, &config)?;
        self.update_provider(role, PROVIDER_REDMINE)
    }

    /// Load the GitLab-specific configuration for `role`. Mirrors the
    /// Redmine helper except the persisted `project_id` is a numeric
    /// GitLab identifier, not a free-text slug.
    pub fn load_gitlab_config(&self, role: Role) -> Result<Option<GitlabStoredConfig>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT api_base, project_id FROM role_gitlab_config WHERE role = ?1")
            .map_err(|error| format!("could not prepare gitlab config load: {error}"))?;
        let value = statement
            .query_row(params![role.as_str()], |row| {
                Ok(GitlabStoredConfig {
                    api_base: row.get(0)?,
                    project_id: row.get::<_, Option<i64>>(1)?.map(|value| value as u64),
                })
            })
            .optional()
            .map_err(|error| format!("could not read gitlab config: {error}"))?;
        Ok(value)
    }

    /// Upsert the GitLab-specific configuration. The numeric project id
    /// is stored as `INTEGER` so the column never holds a placeholder
    /// string that callers might confuse with a Redmine slug.
    pub fn save_gitlab_config(
        &self,
        role: Role,
        config: &GitlabStoredConfig,
    ) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin gitlab config write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_gitlab_config (role, api_base, project_id) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(role) DO UPDATE SET \
                    api_base = excluded.api_base, \
                    project_id = excluded.project_id",
                params![
                    role.as_str(),
                    config.api_base,
                    config.project_id.map(|value| value as i64),
                ],
            )
            .map_err(|error| format!("could not write gitlab config: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit gitlab config write: {error}"))?;
        Ok(())
    }

    /// Persist the bootstrap identity (`api_base` + numeric project id)
    /// without disturbing an existing provider preference on
    /// `role_config`. The project id is required to be greater than
    /// zero because GitLab identifiers are positive integers, and the
    /// bootstrap always flips `role_config.provider` to "gitlab" so the
    /// resolver doesn't drift back to the default Forgejo path.
    pub fn persist_gitlab_bootstrap(
        &self,
        role: Role,
        api_base: Option<String>,
        project_id: u64,
    ) -> Result<(), String> {
        if project_id == 0 {
            return Err("GitLab project id must be greater than zero".to_owned());
        }
        let mut config = self.load_gitlab_config(role)?.unwrap_or_default();
        if api_base.is_some() {
            config.api_base = api_base;
        }
        config.project_id = Some(project_id);
        self.save_gitlab_config(role, &config)?;
        self.update_provider(role, PROVIDER_GITLAB)
    }

    /// Read the credential stored for `(role, provider)`. Returns
    /// `Ok(None)` when no credential exists so the caller can prompt
    /// the operator instead of failing on a missing row.
    pub fn load_credential(&self, role: Role, provider: &str) -> Result<Option<String>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT credential FROM role_credential WHERE role = ?1 AND provider = ?2")
            .map_err(|error| format!("could not prepare credential load: {error}"))?;
        let value = statement
            .query_row(params![role.as_str(), provider], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(|error| format!("could not read credential: {error}"))?;
        Ok(value)
    }

    /// Store the credential for `(role, provider)`, overwriting any
    /// existing value. The credential is stored verbatim and never
    /// surfaced in errors; callers are responsible for trimming and
    /// rejecting empty input before invoking this method.
    pub fn save_credential(
        &self,
        role: Role,
        provider: &str,
        credential: &str,
    ) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin credential write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_credential (role, provider, credential) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(role, provider) DO UPDATE SET credential = excluded.credential",
                params![role.as_str(), provider, credential],
            )
            .map_err(|error| format!("could not write credential: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit credential write: {error}"))?;
        Ok(())
    }

    /// Wipe the credential for `(role, provider)`. Used by tests and
    /// not currently called from production code; left in the public
    /// surface so the storage layer stays self-contained.
    #[allow(dead_code)]
    pub fn delete_credential(&self, role: Role, provider: &str) -> Result<(), String> {
        self.connection
            .execute(
                "DELETE FROM role_credential WHERE role = ?1 AND provider = ?2",
                params![role.as_str(), provider],
            )
            .map_err(|error| format!("could not delete credential: {error}"))?;
        Ok(())
    }

    /// Read the stored value for a global setting. Returns
    /// `Ok(None)` when no row exists or the stored value is empty so
    /// callers can keep the env-over-SQLite precedence uniform.
    pub fn load_global_setting(&self, name: &str) -> Result<Option<String>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT value FROM global_setting WHERE name = ?1")
            .map_err(|error| format!("could not prepare global setting load: {error}"))?;
        let value = statement
            .query_row(params![name], |row| row.get::<_, String>(0))
            .optional()
            .map_err(|error| format!("could not read global setting: {error}"))?;
        Ok(value.and_then(|value| {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }))
    }

    /// Upsert a global setting. Empty values are stored as SQL `NULL`
    /// so `load_global_setting` can keep distinguishing "never
    /// written" from "written with empty value".
    pub fn save_global_setting(&self, name: &str, value: &str) -> Result<(), String> {
        let trimmed = value.trim();
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin global setting write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO global_setting (name, value) VALUES (?1, ?2) \
                 ON CONFLICT(name) DO UPDATE SET value = excluded.value",
                params![name, trimmed],
            )
            .map_err(|error| format!("could not write global setting: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit global setting write: {error}"))?;
        Ok(())
    }

    /// Summarise the canonical set of global settings. The struct
    /// intentionally never carries the full secret value; `config
    /// show` consumes the summaries to render redacted metadata.
    pub fn summarise_global_settings(&self) -> Result<Vec<GlobalSettingSummary>, String> {
        let mut summaries = Vec::with_capacity(GLOBAL_SETTING_NAMES.len());
        for &name in GLOBAL_SETTING_NAMES {
            let stored = self.load_global_setting(name)?;
            summaries.push(match stored {
                Some(value) => GlobalSettingSummary {
                    name,
                    present: true,
                    length: value.chars().count(),
                },
                None => GlobalSettingSummary::missing(name),
            });
        }
        Ok(summaries)
    }

    /// Remove the row for a global setting. Returns `true` when a row
    /// was actually deleted so callers can distinguish "cleared an
    /// existing default" from "no-op because the default was already
    /// absent". Used by `config provider clear` so the persisted
    /// default is removed rather than stored as a confusing empty
    /// value the resolver would later misinterpret.
    pub fn delete_global_setting(&self, name: &str) -> Result<bool, String> {
        let deleted = self
            .connection
            .execute("DELETE FROM global_setting WHERE name = ?1", params![name])
            .map_err(|error| format!("could not delete global setting: {error}"))?;
        Ok(deleted > 0)
    }

    /// Describe the credential stored for `(role, provider)` without
    /// surfacing the value itself. Reports presence and length so
    /// `config show` can render a redacted snapshot of every role.
    pub fn credential_summary(&self, role: Role, provider: &str) -> Result<(bool, usize), String> {
        match self.load_credential(role, provider)? {
            Some(value) => Ok((true, value.chars().count())),
            None => Ok((false, 0)),
        }
    }

    /// Load one execution-ledger row by its caller-supplied run id.
    pub fn load_timer_run(&self, run_id: &str) -> Result<Option<TimerRun>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT run_id, issue_id, phase, role, attempt, started_at, finished_at, status, \
                        elapsed_seconds, rounded_hours, activity_id, redmine_time_entry_id, \
                        sync_status, sync_error, owner_session_id, owner_call_id, \
                        projection_token, projection_claimed_at \
                 FROM execution_timer_runs WHERE run_id = ?1",
            )
            .map_err(|error| format!("could not prepare timer run load: {error}"))?;
        statement
            .query_row(params![run_id], timer_run_from_row)
            .optional()
            .map_err(|error| format!("could not read timer run: {error}"))
    }

    /// List execution-ledger rows filtered by `status`. The result is
    /// ordered newest-started-first so callers can scan the open orphans
    /// without a separate query. Secrets are never part of the projection.
    pub fn list_timer_runs(
        &self,
        status: TimerStatusFilter,
        limit: u32,
    ) -> Result<Vec<TimerRun>, String> {
        // Clamp the cap to keep the bounded JSON output small and to
        // stop a runaway listing from spending its time on rows nobody
        // asked for. The default 100 rows keeps the recovery workflow
        // observable without paging logic.
        let clamped = limit.clamp(1, 1_000);
        let sql = match status {
            TimerStatusFilter::Running => {
                "SELECT run_id, issue_id, phase, role, attempt, started_at, finished_at, status, \
                        elapsed_seconds, rounded_hours, activity_id, redmine_time_entry_id, \
                        sync_status, sync_error, owner_session_id, owner_call_id, \
                        projection_token, projection_claimed_at \
                 FROM execution_timer_runs \
                 WHERE status = 'running' \
                 ORDER BY started_at DESC LIMIT ?1"
            }
            TimerStatusFilter::Finished => {
                "SELECT run_id, issue_id, phase, role, attempt, started_at, finished_at, status, \
                        elapsed_seconds, rounded_hours, activity_id, redmine_time_entry_id, \
                        sync_status, sync_error, owner_session_id, owner_call_id, \
                        projection_token, projection_claimed_at \
                 FROM execution_timer_runs \
                 WHERE status <> 'running' \
                 ORDER BY started_at DESC LIMIT ?1"
            }
            TimerStatusFilter::All => {
                "SELECT run_id, issue_id, phase, role, attempt, started_at, finished_at, status, \
                        elapsed_seconds, rounded_hours, activity_id, redmine_time_entry_id, \
                        sync_status, sync_error, owner_session_id, owner_call_id, \
                        projection_token, projection_claimed_at \
                 FROM execution_timer_runs \
                 ORDER BY started_at DESC LIMIT ?1"
            }
        };
        let mut statement = self
            .connection
            .prepare(sql)
            .map_err(|error| format!("could not prepare timer run list: {error}"))?;
        let rows = statement
            .query_map(params![clamped as i64], timer_run_from_row)
            .map_err(|error| format!("could not read timer run list: {error}"))?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row.map_err(|error| format!("could not decode timer row: {error}"))?);
        }
        Ok(runs)
    }

    /// Persist the start of one wall-clock run. Repeating the same run id and
    /// identity is a no-op; a different identity or an already-finished run is
    /// rejected before any remote operation is attempted. The legacy
    /// six-argument signature preserves backward compatibility with the
    /// Phase 5A callers; new code should prefer
    /// [`start_timer_run_with_owner`] when owner metadata is available.
    #[allow(dead_code)]
    pub fn start_timer_run(
        &self,
        run_id: &str,
        issue: u64,
        phase: &str,
        role: &str,
        attempt: u64,
        started_at: i64,
    ) -> Result<TimerRun, String> {
        self.start_timer_run_with_owner(
            run_id,
            issue,
            phase,
            role,
            attempt,
            started_at,
            &TimerRunOwner::default(),
        )
    }

    /// Persist the start of one wall-clock run and optionally record the
    /// OpenCode session / call identifiers that own it. The owner columns
    /// are nullable so older callers and migrations leave them null
    /// without breaking the row shape.
    #[allow(clippy::too_many_arguments)]
    pub fn start_timer_run_with_owner(
        &self,
        run_id: &str,
        issue: u64,
        phase: &str,
        role: &str,
        attempt: u64,
        started_at: i64,
        owner: &TimerRunOwner,
    ) -> Result<TimerRun, String> {
        validate_timer_identity(run_id, issue, phase, role, attempt)?;
        let owner_session_id =
            validate_owner_field(owner.session_id.as_deref(), "owner_session_id")?;
        let owner_call_id = validate_owner_field(owner.call_id.as_deref(), "owner_call_id")?;
        if let Some(existing) = self.load_timer_run(run_id)? {
            ensure_same_timer_identity(&existing, issue, phase, role, attempt)?;
            if existing.status != TIMER_STATUS_RUNNING {
                return Err(format!("timer run '{run_id}' is already finished"));
            }
            if existing.started_at != started_at {
                return Err(format!(
                    "timer run '{run_id}' was already started at a different time"
                ));
            }
            // Re-attaching an owner to an existing running row is a no-op
            // when the new values match; a mismatch on either field
            // surfaces as a structured error so two competing calls do
            // not silently overwrite each other.
            if owner_session_id.is_some()
                && existing.owner_session_id.is_some()
                && existing.owner_session_id != owner_session_id
            {
                return Err(format!(
                    "timer run '{run_id}' is already owned by another session"
                ));
            }
            if owner_call_id.is_some()
                && existing.owner_call_id.is_some()
                && existing.owner_call_id != owner_call_id
            {
                return Err(format!(
                    "timer run '{run_id}' is already owned by another call"
                ));
            }
            return Ok(existing);
        }

        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin timer start: {error}"))?;
        transaction
            .execute(
                "INSERT INTO execution_timer_runs \
                    (run_id, issue_id, phase, role, attempt, started_at, status, \
                     owner_session_id, owner_call_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    run_id,
                    issue,
                    phase,
                    role,
                    attempt as i64,
                    started_at,
                    TIMER_STATUS_RUNNING,
                    owner_session_id,
                    owner_call_id,
                ],
            )
            .map_err(|error| format!("could not persist timer start: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit timer start: {error}"))?;
        self.load_timer_run(run_id)?
            .ok_or_else(|| "timer start row disappeared after commit".to_owned())
    }

    /// Persist the finish transition and compute exact seconds plus the
    /// independently rounded Redmine hours. The operation is idempotent:
    /// retrying with the same result returns the same finished row and does
    /// not reopen or duplicate the phase.
    pub fn finish_timer_run(
        &self,
        run_id: &str,
        result: &str,
        finished_at: i64,
    ) -> Result<TimerRun, String> {
        if !["DONE", "PARTIAL", "BLOCKED", "FAILED"].contains(&result) {
            return Err(format!("invalid timer result '{result}'"));
        }
        let existing = self
            .load_timer_run(run_id)?
            .ok_or_else(|| format!("timer run '{run_id}' was not found"))?;
        // `status` is the result literal after a successful finish (DONE,
        // PARTIAL, BLOCKED, FAILED) and 'running' while the row is open.
        // A retry with the same result is the idempotent path; a retry
        // with a different result on an already-finished row is rejected
        // so a recovered orphan cannot silently overwrite the original.
        if existing.status != TIMER_STATUS_RUNNING {
            if existing.status == result {
                return Ok(existing);
            }
            return Err(format!(
                "timer run '{run_id}' is already finished as {}; \
                 cannot re-finish as {result}",
                existing.status
            ));
        }
        if finished_at < existing.started_at {
            return Err("timer finish time must not precede its start time".to_owned());
        }
        let elapsed = (finished_at - existing.started_at).max(0);
        let rounded = crate::time_tracking_cli::rounded_hours(elapsed);

        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin timer finish: {error}"))?;
        // `sync_status` is derived from the result + the durable state:
        // - `DONE`/`PARTIAL`/`BLOCKED` follow the time-entry-id rule so a
        //   successful Redmine projection that already linked an id
        //   keeps the row at `synced`; without an id the row needs a
        //   projection retry and stays at `pending`.
        // - `FAILED` always sets `sync_status='failed'` so the orphan is
        //   durably marked locally before any provider attempt. The
        //   `timer recover` path depends on this to satisfy the
        //   "durable local FAILED before provider/config lookup"
        //   invariant without an unconditional fallback in the failure
        //   path of `execute_finish`/`execute_recovery`.
        transaction
            .execute(
                "UPDATE execution_timer_runs \
                 SET finished_at = ?2, status = ?3, elapsed_seconds = ?4, rounded_hours = ?5, \
                     activity_id = COALESCE(activity_id, ?6), \
                     sync_status = CASE WHEN ?3 = 'FAILED' THEN 'failed' \
                                       WHEN redmine_time_entry_id IS NOT NULL \
                                       THEN 'synced' ELSE 'pending' END, \
                     sync_error = NULL \
                 WHERE run_id = ?1 AND status = 'running'",
                params![
                    run_id,
                    finished_at,
                    result,
                    elapsed,
                    rounded,
                    Option::<u64>::None,
                ],
            )
            .map_err(|error| format!("could not persist timer finish: {error}"))?;
        if transaction.changes() != 1 {
            return Err("timer finish lost its running row during update".to_owned());
        }
        transaction
            .commit()
            .map_err(|error| format!("could not commit timer finish: {error}"))?;
        self.load_timer_run(run_id)?
            .ok_or_else(|| "timer finish row disappeared after commit".to_owned())
    }

    /// Atomically claim the projection for one run with a caller-bound
    /// lease token. Only a row whose `sync_status` is `pending`,
    /// `failed`, or `unconfirmed` can be claimed; the update moves it to
    /// `projecting` and records the token plus claimed_at. The caller must
    /// present the same token to finalize (`synced`/`failed`/`unconfirmed`)
    /// and to explicitly reset. A loaded `projecting` row without the
    /// matching token is never considered this caller's claim. Legacy rows
    /// with NULL token/claimed_at are treated as unclaimed until migrated;
    /// `try_claim` will succeed on them if their sync status is retryable.
    pub fn try_claim_timer_projection(&self, run_id: &str, token: &str) -> Result<bool, String> {
        validate_projection_token(token)?;
        let now = now_epoch_seconds();
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET sync_status = ?1, projection_token = ?2, projection_claimed_at = ?3 \
                 WHERE run_id = ?4 \
                   AND (sync_status = ?5 OR sync_status = ?6 OR sync_status = ?7)",
                params![
                    TIMER_SYNC_PROJECTING,
                    token,
                    now,
                    run_id,
                    TIMER_SYNC_PENDING,
                    TIMER_SYNC_FAILED,
                    TIMER_SYNC_UNCONFIRMED
                ],
            )
            .map_err(|error| format!("could not claim timer projection: {error}"))?;
        Ok(changed == 1)
    }

    /// Reset a `projecting` row back to `failed` when the caller presents
    /// the matching lease token. The token check guarantees a concurrent
    /// live projector cannot be reset by a second caller. Use
    /// `reset_stale_projection_to_failed` for the hard-crash recovery path
    /// where the original token is no longer available.
    #[allow(dead_code)]
    pub fn reset_projecting_to_failed(
        &self,
        run_id: &str,
        token: &str,
        error: &str,
    ) -> Result<bool, String> {
        validate_projection_token(token)?;
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET sync_status = ?1, sync_error = ?2, projection_token = NULL, projection_claimed_at = NULL \
                 WHERE run_id = ?3 AND sync_status = ?4 AND projection_token = ?5",
                params![TIMER_SYNC_FAILED, error, run_id, TIMER_SYNC_PROJECTING, token],
            )
            .map_err(|error| format!("could not reset projecting timer: {error}"))?;
        Ok(changed == 1)
    }

    /// Force-reset a stale `projecting` claim that is older than the lease
    /// window or has a NULL claimed_at (legacy). This is the explicit
    /// `timer recover` recovery path for a hard-crash orphan: the caller
    /// does not hold the token but can recover after the lease expires.
    /// A live projector whose claim is still within the lease is not reset.
    ///
    /// # Liveness invariant and legacy compatibility
    ///
    /// The reset acquires an `IMMEDIATE` SQLite transaction first so it
    /// cannot race against a live projector that is also holding one.
    /// While the live projector holds its `IMMEDIATE` (from claim through
    /// token-bound finalization), the reset blocks on `busy`/`locked`,
    /// observes the row state after the live projector commits or rolls
    /// back, and never observes a mid-flight `projecting` row owned by
    /// another caller. The wall-clock check is therefore enforced in
    /// addition to the lock so the reset only fires on a truly abandoned
    /// row (live crash that left an autocommit or legacy `NULL` claim).
    /// A modern hard crash that was inside a held transaction rolls the
    /// `projecting` claim back to the pre-claim state on process exit, so
    /// no stale row remains for this reset to discover; only legacy or
    /// autocommit crash windows leave a `projecting` row recoverable.
    ///
    /// # Return shape
    ///
    /// Returns `Ok(true)` when the row was reset, `Ok(false)` when the
    /// row is held by a live lease inside a held `IMMEDIATE` (and the
    /// caller should surface "projection already in progress"), and
    /// `Ok(false)` when the row is no longer in the `projecting` state.
    /// Returns `Err` only on storage failures.
    pub fn reset_stale_projection_to_failed(
        &self,
        run_id: &str,
        error: &str,
    ) -> Result<bool, String> {
        // Acquire `IMMEDIATE` first so the reset blocks against a live
        // holder that is also inside an `IMMEDIATE`. The held lock is
        // the liveness signal; time alone never makes a live holder
        // stealable. We retry `busy`/`locked` with bounded backoff so a
        // legitimate wait for the live holder's commit or rollback is
        // not falsely reported as a reset failure.
        self.begin_projection()?;
        let now = now_epoch_seconds();
        let threshold = now - PROJECTION_LEASE_SECS;
        let outcome: Result<bool, String> = (|| {
            // Re-read inside the held IMMEDIATE so we observe the
            // post-commit/post-rollback state of any concurrent live
            // holder. The lease window check is the documented hard-
            // crash legacy path; a live holder that survived its
            // transaction is by definition outside this window only
            // after it has either succeeded (sync_status != projecting)
            // or rolled back (sync_status != projecting).
            let changed = self
                .connection
                .execute(
                    "UPDATE execution_timer_runs \
                     SET sync_status = ?1, sync_error = ?2, projection_token = NULL, projection_claimed_at = NULL \
                     WHERE run_id = ?3 AND sync_status = ?4 \
                       AND (projection_claimed_at IS NULL OR projection_claimed_at <= ?5)",
                    params![
                        TIMER_SYNC_FAILED,
                        error,
                        run_id,
                        TIMER_SYNC_PROJECTING,
                        threshold
                    ],
                )
                .map_err(|error| format!("could not reset stale projecting timer: {error}"))?;
            Ok(changed == 1)
        })();
        match outcome {
            Ok(value) => {
                self.commit_projection()?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.rollback_projection();
                Err(error)
            }
        }
    }

    /// Acquire an `IMMEDIATE` transaction that serializes the entire
    /// projection (claim, activity lookup, re-list, POST, finalization).
    /// While held, a concurrent `BEGIN IMMEDIATE` on another connection
    /// receives `busy`/`locked` and must surface "already in progress"
    /// without POST. The lock is released by `commit_projection` or
    /// `rollback_projection`. Retries `busy` a few times before surfacing.
    pub(crate) fn begin_projection(&self) -> Result<(), String> {
        let mut attempts = 0;
        loop {
            match self.connection.execute("BEGIN IMMEDIATE", []) {
                Ok(_) => return Ok(()),
                Err(error) => {
                    let msg = error.to_string().to_ascii_lowercase();
                    let is_busy = msg.contains("busy") || msg.contains("locked");
                    attempts += 1;
                    if is_busy && attempts < 5 {
                        std::thread::sleep(std::time::Duration::from_millis(10 * attempts as u64));
                        continue;
                    }
                    return Err(format!("could not acquire projection lock: {error}"));
                }
            }
        }
    }

    pub(crate) fn commit_projection(&self) -> Result<(), String> {
        self.connection
            .execute("COMMIT", [])
            .map_err(|error| format!("could not commit projection: {error}"))?;
        Ok(())
    }

    pub(crate) fn rollback_projection(&self) -> Result<(), String> {
        let _ = self.connection.execute("ROLLBACK", []);
        Ok(())
    }

    /// Persist `activity_id` while holding the projection lease. The row
    /// must still be `projecting` and the `token` must match, so a
    /// concurrent holder cannot overwrite the activity.
    pub(crate) fn update_activity_with_token(
        &self,
        run_id: &str,
        token: &str,
        activity_id: u64,
    ) -> Result<bool, String> {
        validate_projection_token(token)?;
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET activity_id = ?2 \
                 WHERE run_id = ?1 AND projection_token = ?3 AND sync_status = ?4",
                params![run_id, activity_id as i64, token, TIMER_SYNC_PROJECTING],
            )
            .map_err(|error| format!("could not update activity with token: {error}"))?;
        Ok(changed == 1)
    }

    /// Finalize a claimed projection when the caller holds the lease token.
    /// The update succeeds only when `projection_token` matches and the row
    /// is still `projecting`. On transition out of `projecting` the token
    /// and claimed_at are cleared so a retry can claim again. Callers that
    /// do not hold the token receive `Ok(false)` semantics via the
    /// `changes == 0` path and should surface "projection already in
    /// progress" rather than overwriting another claim.
    pub fn mark_timer_sync_with_token(
        &self,
        run_id: &str,
        token: &str,
        activity_id: Option<u64>,
        time_entry_id: Option<u64>,
        sync_status: &str,
        sync_error: Option<&str>,
    ) -> Result<bool, String> {
        if !valid_timer_sync_status(sync_status) {
            return Err(format!("invalid timer sync status '{sync_status}'"));
        }
        if sync_status == TIMER_SYNC_FAILED && sync_error.is_none_or(str::is_empty) {
            return Err("timer sync failure requires a non-empty error".to_owned());
        }
        validate_projection_token(token)?;
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET activity_id = COALESCE(activity_id, ?2), \
                     redmine_time_entry_id = COALESCE(redmine_time_entry_id, ?3), \
                     sync_status = ?4, sync_error = ?5, \
                     projection_token = CASE WHEN ?4 IN ('synced','failed','unconfirmed') THEN NULL ELSE projection_token END, \
                     projection_claimed_at = CASE WHEN ?4 IN ('synced','failed','unconfirmed') THEN NULL ELSE projection_claimed_at END \
                 WHERE run_id = ?1 AND projection_token = ?6 AND sync_status = ?7",
                params![
                    run_id,
                    activity_id,
                    time_entry_id,
                    sync_status,
                    sync_error,
                    token,
                    TIMER_SYNC_PROJECTING
                ],
            )
            .map_err(|error| format!("could not persist timer sync with token: {error}"))?;
        Ok(changed == 1)
    }

    /// Record `sync_error` on a row whose local finish is already
    /// `FAILED`. This is the documented durable local FAILED step used
    /// by `timer recover` after a projection attempt fails: the row's
    /// `status` is `FAILED` (set by `finish_timer_run`) and its
    /// `sync_status` is `failed` (also set there) until a future
    /// successful claim. The update only fires when the row is in a
    /// state the caller exclusively owns (`failed` is not `projecting`)
    /// so a concurrent live holder is never overwritten. The error
    /// message is bounded to keep audit logs compact.
    pub fn record_failed_sync_error(&self, run_id: &str, error: &str) -> Result<bool, String> {
        let trimmed = error.chars().take(512).collect::<String>();
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET sync_error = ?2 \
                 WHERE run_id = ?1 AND status IN ('FAILED', 'DONE', 'PARTIAL', 'BLOCKED') \
                   AND sync_status != ?3",
                params![run_id, trimmed, TIMER_SYNC_PROJECTING],
            )
            .map_err(|error| format!("could not record failed sync error: {error}"))?;
        Ok(changed == 1)
    }

    /// Advance the Redmine synchronization state after a local finish.  The
    /// coalescing assignments make retries safe even when a caller has only
    /// an activity id or only a time-entry id. Production callers should
    /// prefer `mark_timer_sync_with_token` so the transition is bound to
    /// the lease holder; this entry point remains `pub` for tests and
    /// external integrations that explicitly opt out of the lease
    /// protocol (and therefore accept the corresponding concurrency
    /// risk).
    #[allow(dead_code)]
    pub fn mark_timer_sync(
        &self,
        run_id: &str,
        activity_id: Option<u64>,
        time_entry_id: Option<u64>,
        sync_status: &str,
        sync_error: Option<&str>,
    ) -> Result<TimerRun, String> {
        if !valid_timer_sync_status(sync_status) {
            return Err(format!("invalid timer sync status '{sync_status}'"));
        }
        if sync_status == TIMER_SYNC_FAILED && sync_error.is_none_or(str::is_empty) {
            return Err("timer sync failure requires a non-empty error".to_owned());
        }
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin timer sync: {error}"))?;
        transaction
            .execute(
                "UPDATE execution_timer_runs \
                 SET activity_id = COALESCE(activity_id, ?2), \
                     redmine_time_entry_id = COALESCE(redmine_time_entry_id, ?3), \
                     sync_status = ?4, sync_error = ?5 \
                 WHERE run_id = ?1",
                params![run_id, activity_id, time_entry_id, sync_status, sync_error],
            )
            .map_err(|error| format!("could not persist timer sync: {error}"))?;
        if transaction.changes() != 1 {
            return Err(format!("timer run '{run_id}' was not found"));
        }
        transaction
            .commit()
            .map_err(|error| format!("could not commit timer sync: {error}"))?;
        self.load_timer_run(run_id)?
            .ok_or_else(|| "timer sync row disappeared after commit".to_owned())
    }
}

/// Create `path` with private permissions. Used for both the SQLite
/// database directory and the database file itself; on non-Unix
/// platforms the directory/file is created with default permissions
/// and only the SQLite file-mode pragma handles visibility.
fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("could not create phasegent config directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("could not secure phasegent config directory: {error}"))?;
    }
    Ok(())
}

/// Resolve the canonical phasegent database path via
/// [`directories::ProjectDirs`]. The qualifier / organisation / application
/// tuple (`com` / `Cloud1ful` / `phasegent`) maps to the platform-standard
/// config directory:
/// - Linux: `$XDG_CONFIG_HOME/phasegent` (defaults to `~/.config/phasegent`)
/// - macOS: `~/Library/Application Support/com.Cloud1ful.phasegent`
/// - Windows: `%APPDATA%\Cloud1ful\phasegent\config`
fn project_dirs_db_path() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("com", "Cloud1ful", "phasegent")
        .ok_or_else(|| "could not resolve phasegent config directory".to_owned())?;
    Ok(dirs.config_dir().join(DB_FILENAME))
}

fn timer_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TimerRun> {
    Ok(TimerRun {
        run_id: row.get(0)?,
        issue: row.get::<_, i64>(1)? as u64,
        phase: row.get(2)?,
        role: row.get(3)?,
        attempt: row.get::<_, i64>(4)? as u64,
        started_at: row.get(5)?,
        finished_at: row.get(6)?,
        status: row.get(7)?,
        elapsed_seconds: row.get(8)?,
        rounded_hours: row.get(9)?,
        activity_id: row.get::<_, Option<i64>>(10)?.map(|value| value as u64),
        time_entry_id: row.get::<_, Option<i64>>(11)?.map(|value| value as u64),
        sync_status: row.get(12)?,
        sync_error: row.get(13)?,
        owner_session_id: row.get(14)?,
        owner_call_id: row.get(15)?,
        projection_token: row.get(16)?,
        projection_claimed_at: row.get(17)?,
    })
}

/// True when `column` already exists on `table` so the additive migration
/// step can skip it without re-running `ALTER TABLE ... ADD COLUMN`.
fn column_exists(connection: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("could not inspect {table} columns: {error}"))?;
    let mut rows = statement
        .query([])
        .map_err(|error| format!("could not read {table} columns: {error}"))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| format!("could not advance {table} columns: {error}"))?
    {
        let name: String = row
            .get(1)
            .map_err(|error| format!("could not read column name: {error}"))?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_timer_identity(
    run_id: &str,
    issue: u64,
    phase: &str,
    role: &str,
    attempt: u64,
) -> Result<(), String> {
    if run_id.trim().is_empty() || run_id.chars().count() > 128 {
        return Err("timer run id must be a non-empty value of at most 128 characters".to_owned());
    }
    if run_id.chars().any(char::is_control) {
        return Err("timer run id must not contain control characters".to_owned());
    }
    if issue == 0 {
        return Err("timer issue id must be greater than zero".to_owned());
    }
    if phase.trim().is_empty() || phase.chars().count() > 128 {
        return Err("timer phase must be a non-empty value of at most 128 characters".to_owned());
    }
    if phase.chars().any(char::is_control) {
        return Err("timer phase must not contain control characters".to_owned());
    }
    if !matches!(role, "executor" | "reviewer") {
        return Err("timer agent role must be executor or reviewer".to_owned());
    }
    if attempt == 0 || attempt > i64::MAX as u64 {
        return Err("timer attempt must be between 1 and i64::MAX".to_owned());
    }
    Ok(())
}

/// Bound and sanitize an optional owner metadata field. `None` and empty
/// values collapse to `None` so the column stores SQL `NULL` rather than
/// an empty string. Non-empty values are bounded to 128 characters and
/// stripped of control bytes.
fn validate_owner_field(value: Option<&str>, name: &str) -> Result<Option<String>, String> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.chars().count() > 128 {
                return Err(format!("{name} must be at most 128 characters"));
            }
            if trimmed.chars().any(char::is_control) {
                return Err(format!("{name} must not contain control characters"));
            }
            Ok(Some(trimmed.to_owned()))
        }
    }
}

fn ensure_same_timer_identity(
    run: &TimerRun,
    issue: u64,
    phase: &str,
    role: &str,
    attempt: u64,
) -> Result<(), String> {
    if run.issue != issue || run.phase != phase || run.role != role || run.attempt != attempt {
        return Err(format!(
            "timer run '{}' was already used for a different phase identity",
            run.run_id
        ));
    }
    Ok(())
}

fn validate_projection_token(token: &str) -> Result<(), String> {
    if token.trim().is_empty() || token.chars().count() > PROJECTION_TOKEN_BOUND {
        return Err(format!(
            "projection token must be 1..{PROJECTION_TOKEN_BOUND} characters"
        ));
    }
    if token.chars().any(char::is_control) {
        return Err("projection token must not contain control characters".to_owned());
    }
    Ok(())
}

fn now_epoch_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

/// Test-only helpers shared between `storage_tests` and the
/// `redmine_contract_tests` mirror-env tests so they serialise
/// against the same process-wide lock before mutating global
/// environment state. Production builds omit this module entirely
/// because every item is gated behind `#[cfg(test)]`.
#[cfg(test)]
pub(crate) mod test_support {
    use std::env;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn workflow_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Acquire the workflow test lock while tolerating a previous
    /// panic that poisoned it. The mirror plugin contract tests and
    /// the storage non-persistence test both mutate
    /// `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` and
    /// `PHASEGENT_REDMINE_REPOSITORY_URL`; without this mutex the
    /// two test groups can race under the default `cargo test`
    /// parallel runner and the contract test's bearer-key assertion
    /// would observe a value installed by the storage test.
    pub(crate) fn lock_workflow_tests() -> MutexGuard<'static, ()> {
        let mutex = workflow_test_lock();
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// RAII guard that sets an environment variable for the duration
    /// of its scope and restores the previous value on Drop. Tests
    /// that mutate process-wide state must use this helper so they
    /// leave the host shell with the same environment they found.
    pub(crate) struct EnvGuard {
        name: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        /// Capture the current value of `name` (if any), install
        /// `value`, and return a guard whose Drop restores the
        /// original state. The previous value is never copied into
        /// the test output.
        pub(crate) fn set(name: &'static str, value: &str) -> Self {
            let previous = env::var_os(name);
            // SAFETY::`set_var`/`remove_var` are unsafe in this
            // toolchain; tests serialise on `lock_workflow_tests()`
            // so no other thread can observe the transient state.
            unsafe {
                env::set_var(name, value);
            }
            Self { name, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY::Symmetric to `set_var` above; the lock guard
            // is still held when the test stack unwinds.
            unsafe {
                if let Some(previous) = self.previous.take() {
                    env::set_var(self.name, previous);
                } else {
                    env::remove_var(self.name);
                }
            }
        }
    }
}
