use crate::infra::storage_schema::DB_FILENAME;
use crate::infra::storage_schema::{MIGRATIONS, PRAGMA_STATEMENTS, SCHEMA};
use directories::ProjectDirs;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

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
            // Phase 1 (remove-project-id): clear legacy project_id values
            // from both Redmine and GitLab tables. The columns remain for
            // non-destructive, compatibility-safe migration, but values
            // must be inert. The updates are idempotent and run inside
            // the same IMMEDIATE transaction that protects the column
            // migrations.
            for (table, column) in [
                ("role_redmine_config", "project_id"),
                ("role_gitlab_config", "project_id"),
            ] {
                if column_exists(connection, table, column)? {
                    let statement =
                        format!("UPDATE {table} SET {column} = NULL WHERE {column} IS NOT NULL");
                    // UPDATE never fails on empty table; ignore duplicate-column
                    // noise and propagate any real error.
                    if let Err(error) = connection.execute(&statement, []) {
                        let message = error.to_string();
                        if message.contains("no such column") || message.contains("no such table") {
                            continue;
                        }
                        return Err(format!("could not clear legacy {table}.{column}: {error}"));
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
