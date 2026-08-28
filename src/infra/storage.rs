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
use crate::infra::storage_schema::{GLOBAL_SETTING_NAMES, MIGRATIONS, PRAGMA_STATEMENTS, SCHEMA};
use crate::policy::Role;
use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) use crate::infra::storage_schema::{
    DB_FILENAME, PROVIDER_FORGEJO, PROVIDER_GITLAB, PROVIDER_REDMINE,
};

/// Re-export the canonical global setting names so callers do not
// need to depend on `storage_schema` directly.
pub(crate) use crate::infra::storage_schema::{
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

#[allow(unused_imports)]
pub(crate) use crate::infra::timer_ledger::{
    PROJECTION_LEASE_SECS, PROJECTION_TOKEN_BOUND, TIMER_STATUS_RUNNING, TIMER_SYNC_FAILED,
    TIMER_SYNC_PENDING, TIMER_SYNC_PROJECTING, TIMER_SYNC_SYNCED, TIMER_SYNC_UNCONFIRMED,
    valid_timer_sync_status,
};
pub use crate::infra::timer_ledger::{TimerRun, TimerRunOwner, TimerStatusFilter};

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
