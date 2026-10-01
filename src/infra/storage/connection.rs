use crate::infra::sqlite_file;
use crate::infra::storage_schema::DB_FILENAME;
use crate::infra::storage_schema::{
    LEGACY_RESEARCH_CWD_COLUMN, LEGACY_RESEARCH_RUN_OWNERS_OWNER_INDEX,
    LEGACY_RESEARCH_RUN_OWNERS_TABLE, LEGACY_RESEARCH_RUNS_STATUS_INDEX,
    LEGACY_RESEARCH_RUNS_TABLE, MIGRATIONS, PRAGMA_STATEMENTS, RESEARCH_CWD_COLUMN,
    RESEARCH_RUN_OWNERS_TABLE, RESEARCH_RUNS_TABLE, SCHEMA,
};
use rusqlite::Connection;
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
        let path = sqlite_file::project_dirs_config_path(DB_FILENAME)?;
        Self::open_at(&path)
    }

    /// Open the database at an explicit filesystem path. Used by tests
    /// that want full control over the location. Creates the parent
    /// directory with mode 0700 and the database file with mode 0600
    /// on Unix before handing the connection off.
    pub fn open_at(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            sqlite_file::create_private_dir(parent, "phasegent config", false)?;
        }
        let connection = sqlite_file::open_private_connection(path, "phasegent")?;
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
        // The legacy research-ledger rename must run before `SCHEMA`:
        // `SCHEMA` uses `CREATE TABLE IF NOT EXISTS`, so on a pre-issue-692
        // database it would first create a fresh empty `acp_research_runs`
        // and the rename would then find the target occupied while the
        // legacy rows stayed stranded. Renaming first keeps the active
        // pending/running rows, owner records, and values.
        Self::migrate_research_ledger(connection)?;
        connection
            .execute_batch(SCHEMA)
            .map_err(|error| format!("could not initialise phasegent schema: {error}"))?;
        Self::apply_migrations(connection)?;
        Ok(())
    }

    /// Rename the pre-issue-692 research ledger into its canonical names.
    ///
    /// Idempotent and non-destructive: a fresh database (no legacy tables) is
    /// untouched, an already-migrated database is a no-op, and a legacy
    /// database keeps every run row, owner row, and value. The rename, the
    /// column rename, and the old-index drops all run in one `BEGIN IMMEDIATE`
    /// transaction with the same busy-retry and commit/rollback guarantees as
    /// [`Self::apply_migrations`], so a partial rename is never observable.
    fn migrate_research_ledger(connection: &Connection) -> Result<(), String> {
        Self::with_immediate_transaction(connection, |connection| {
            rename_legacy_table(connection, LEGACY_RESEARCH_RUNS_TABLE, RESEARCH_RUNS_TABLE)?;
            rename_legacy_table(
                connection,
                LEGACY_RESEARCH_RUN_OWNERS_TABLE,
                RESEARCH_RUN_OWNERS_TABLE,
            )?;
            if table_exists(connection, RESEARCH_RUNS_TABLE)? {
                rename_column_if_needed(
                    connection,
                    RESEARCH_RUNS_TABLE,
                    LEGACY_RESEARCH_CWD_COLUMN,
                    RESEARCH_CWD_COLUMN,
                )?;
            }
            // SQLite keeps an index name across `ALTER TABLE ... RENAME TO`, so
            // the legacy-named indexes still point at the renamed table. Drop
            // them here; `SCHEMA` then creates the canonical-named ones.
            for index in [
                LEGACY_RESEARCH_RUNS_STATUS_INDEX,
                LEGACY_RESEARCH_RUN_OWNERS_OWNER_INDEX,
            ] {
                connection
                    .execute(&format!("DROP INDEX IF EXISTS {index}"), [])
                    .map_err(|error| format!("could not drop legacy index {index}: {error}"))?;
            }
            Ok(())
        })
    }

    /// Run every additive `MIGRATIONS` row whose column is not yet present on
    /// `execution_timer_runs`. Idempotent across opens so a database
    /// that already carries the column is untouched. Two processes opening
    /// the same pre-owner database concurrently serialize via an immediate
    /// transaction and tolerate a duplicate-column race as success.
    /// `BEGIN IMMEDIATE` is retried on `busy`/`locked` with bounded
    /// backoff; any lock or commit failure is propagated so callers never
    /// observe a successful open when the schema is not durable.
    fn apply_migrations(connection: &Connection) -> Result<(), String> {
        Self::with_immediate_transaction(connection, |connection| {
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
            // Clear legacy project_id values from both Redmine and GitLab
            // tables. The columns remain for non-destructive,
            // compatibility-safe migration, but values must be inert. The
            // updates are idempotent and run inside the same IMMEDIATE
            // transaction that protects the column migrations.
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
        })
    }

    /// Run `run` inside a `BEGIN IMMEDIATE` transaction with the migration
    /// busy-retry and the commit/rollback guarantees. Serializing the
    /// check+DDL is what lets two concurrent opens both succeed instead of
    /// one losing a race; a lock or commit failure is propagated so an open
    /// never reports success while the schema is not durable.
    fn with_immediate_transaction<T>(
        connection: &Connection,
        run: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
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
        match run(connection) {
            Ok(value) => {
                if let Err(error) = connection.execute("COMMIT", []) {
                    let _ = connection.execute("ROLLBACK", []);
                    return Err(format!("could not commit migration: {error}"));
                }
                Ok(value)
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

fn table_exists(connection: &Connection, table: &str) -> Result<bool, String> {
    let mut statement = connection
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
        .map_err(|error| format!("could not inspect schema for {table}: {error}"))?;
    let exists = statement
        .exists(rusqlite::params![table])
        .map_err(|error| format!("could not read schema for {table}: {error}"))?;
    Ok(exists)
}

/// Rename `from` to `to` when only the legacy table exists. A no-op when
/// `from` is already gone (fresh or already-migrated database), and it leaves
/// both tables alone rather than failing if a stray `to` already exists, so the
/// migration can never error out on an unexpected state.
fn rename_legacy_table(connection: &Connection, from: &str, to: &str) -> Result<(), String> {
    if !table_exists(connection, from)? || table_exists(connection, to)? {
        return Ok(());
    }
    connection
        .execute(&format!("ALTER TABLE {from} RENAME TO {to}"), [])
        .map_err(|error| format!("could not rename {from} to {to}: {error}"))?;
    Ok(())
}

/// Rename one column when the legacy name is present and the canonical name is
/// not. Idempotent across opens.
fn rename_column_if_needed(
    connection: &Connection,
    table: &str,
    from: &str,
    to: &str,
) -> Result<(), String> {
    if !column_exists(connection, table, from)? || column_exists(connection, table, to)? {
        return Ok(());
    }
    connection
        .execute(
            &format!("ALTER TABLE {table} RENAME COLUMN {from} TO {to}"),
            [],
        )
        .map_err(|error| format!("could not rename {table}.{from}: {error}"))?;
    Ok(())
}
