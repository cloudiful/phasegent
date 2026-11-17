//! Legacy SQLite configuration source: read, back up, and purge.
//!
//! The importer reads the seven legacy configuration tables of an existing
//! phasegent database, makes a consistent private backup before anything
//! is deleted, and clears only those tables afterwards. The database file
//! itself and every business row stay untouched.
//!
//! Nothing is deleted before the source is corroborated as a known
//! phasegent configuration store: the database must carry the known
//! phasegent business tables, every configuration table must match one of
//! the known schema variants, and the configuration tables must not carry
//! triggers or foreign keys that could reach beyond the cleared rows.

use crate::infra::config_file::{
    LegacyConfigRows, LegacyCredentialRow, LegacyForgejoConfigRow, LegacyGitlabConfigRow,
    LegacyGlobalSettingRow, LegacyRedmineConfigRow, LegacyRedmineUserRow, LegacyRoleConfigRow,
};
use rusqlite::{Connection, OpenFlags};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// The seven legacy configuration tables, in the order the purge reports
/// them.
pub const CONFIG_TABLES: [&str; 7] = [
    "role_config",
    "role_redmine_config",
    "role_forgejo_config",
    "role_gitlab_config",
    "role_credential",
    "role_redmine_user",
    "global_setting",
];

/// A known column of a known phasegent table: its name, its declared type,
/// and whether it takes part in the primary key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KnownColumn {
    name: &'static str,
    ty: &'static str,
    primary_key: bool,
}

/// Build a column list.
macro_rules! columns {
    ($(($name:literal, $ty:literal, $primary_key:literal)),* $(,)?) => {
        &[$(KnownColumn {
            name: $name,
            ty: $ty,
            primary_key: $primary_key,
        }),*]
    };
}

/// The column layout of a table as the database reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ColumnInfo {
    name: String,
    ty: String,
    primary_key: bool,
}

/// The known schema variants of every legacy configuration table.
///
/// The variants are the shapes the configuration tables had in the tracked
/// history: the additive nullable columns (`fingerprint`,
/// `credential_updated_at`, the GitLab provenance columns) were introduced
/// later, so an older store carries fewer columns and is read with the
/// missing columns treated as NULL. A configuration table must match one
/// of its variants exactly: the importer reads it column by column, and a
/// column it does not know would be dropped silently.
const CONFIG_SHAPES: &[(&str, &[&[KnownColumn]])] = &[
    (
        "role_config",
        &[columns![
            ("role", "TEXT", true),
            ("provider", "TEXT", false),
            ("api_base", "TEXT", false),
            ("repository", "TEXT", false),
        ]],
    ),
    (
        "role_redmine_config",
        &[columns![
            ("role", "TEXT", true),
            ("api_base", "TEXT", false),
            ("project_id", "TEXT", false),
            ("close_status_id", "INTEGER", false),
        ]],
    ),
    (
        "role_forgejo_config",
        &[columns![
            ("role", "TEXT", true),
            ("api_base", "TEXT", false),
            ("repository", "TEXT", false),
            ("source", "TEXT", false),
            ("updated_at", "INTEGER", false),
        ]],
    ),
    (
        "role_gitlab_config",
        &[
            columns![
                ("role", "TEXT", true),
                ("api_base", "TEXT", false),
                ("project_id", "INTEGER", false),
            ],
            columns![
                ("role", "TEXT", true),
                ("api_base", "TEXT", false),
                ("project_id", "INTEGER", false),
                ("repository", "TEXT", false),
                ("source", "TEXT", false),
                ("updated_at", "INTEGER", false),
            ],
        ],
    ),
    (
        "role_credential",
        &[
            columns![
                ("role", "TEXT", true),
                ("provider", "TEXT", true),
                ("credential", "TEXT", false),
            ],
            columns![
                ("role", "TEXT", true),
                ("provider", "TEXT", true),
                ("credential", "TEXT", false),
                ("fingerprint", "TEXT", false),
            ],
            columns![
                ("role", "TEXT", true),
                ("provider", "TEXT", true),
                ("credential", "TEXT", false),
                ("fingerprint", "TEXT", false),
                ("credential_updated_at", "INTEGER", false),
            ],
        ],
    ),
    (
        "role_redmine_user",
        &[columns![
            ("role", "TEXT", true),
            ("user_id", "INTEGER", false),
            ("login", "TEXT", false),
        ]],
    ),
    (
        "global_setting",
        &[columns![("name", "TEXT", true), ("value", "TEXT", false)]],
    ),
];

/// The known phasegent business tables.
///
/// These tables are never read or written by the importer; they are the
/// evidence that the source really is a phasegent database. A business
/// table may be an older subset of a known variant, because the columns
/// the later versions added are additive, but it must carry the listed
/// A known business table: its name, the schema variants it may have, and
/// the columns every variant carries.
type BusinessShape = (
    &'static str,
    &'static [&'static [KnownColumn]],
    &'static [&'static str],
);

const BUSINESS_SHAPES: &[BusinessShape] = &[
    (
        "execution_timer_runs",
        &[
            columns![
                ("run_id", "TEXT", true),
                ("issue_id", "INTEGER", false),
                ("phase", "TEXT", false),
                ("role", "TEXT", false),
                ("attempt", "INTEGER", false),
                ("started_at", "INTEGER", false),
                ("finished_at", "INTEGER", false),
                ("status", "TEXT", false),
                ("elapsed_seconds", "INTEGER", false),
                ("rounded_hours", "REAL", false),
                ("activity_id", "INTEGER", false),
                ("redmine_time_entry_id", "INTEGER", false),
                ("sync_status", "TEXT", false),
                ("sync_error", "TEXT", false),
            ],
            columns![
                ("run_id", "TEXT", true),
                ("issue_id", "INTEGER", false),
                ("phase", "TEXT", false),
                ("role", "TEXT", false),
                ("attempt", "INTEGER", false),
                ("started_at", "INTEGER", false),
                ("finished_at", "INTEGER", false),
                ("status", "TEXT", false),
                ("elapsed_seconds", "INTEGER", false),
                ("rounded_hours", "REAL", false),
                ("activity_id", "INTEGER", false),
                ("redmine_time_entry_id", "INTEGER", false),
                ("sync_status", "TEXT", false),
                ("sync_error", "TEXT", false),
                ("owner_session_id", "TEXT", false),
                ("owner_call_id", "TEXT", false),
                ("projection_token", "TEXT", false),
                ("projection_claimed_at", "INTEGER", false),
            ],
        ],
        &[
            "run_id",
            "issue_id",
            "phase",
            "role",
            "attempt",
            "started_at",
            "status",
        ],
    ),
    (
        "notification_deliveries",
        &[columns![
            ("id", "INTEGER", true),
            ("created_at", "INTEGER", false),
            ("event", "TEXT", false),
            ("channel", "TEXT", false),
            ("title", "TEXT", false),
            ("body", "TEXT", false),
            ("issue_id", "INTEGER", false),
            ("status", "TEXT", false),
            ("error", "TEXT", false),
        ]],
        &["id", "created_at", "event", "channel", "status"],
    ),
    (
        "task_bindings",
        &[columns![
            ("task_id", "INTEGER", true),
            ("provider", "TEXT", false),
            ("instance", "TEXT", false),
            ("project", "TEXT", false),
            ("external_id", "TEXT", false),
            ("created_at", "INTEGER", false),
        ]],
        &["task_id", "provider", "instance", "project", "external_id"],
    ),
    (
        "task_repo_memberships",
        &[columns![
            ("repo_key", "TEXT", true),
            ("task_id", "INTEGER", true),
            ("created_at", "INTEGER", false),
        ]],
        &["repo_key", "task_id"],
    ),
    (
        "branch_issue_links",
        &[
            columns![
                ("repo_key", "TEXT", true),
                ("branch", "TEXT", true),
                ("provider", "TEXT", true),
                ("project", "TEXT", true),
                ("external_id", "TEXT", true),
                ("issue_number", "INTEGER", false),
                ("status", "TEXT", false),
                ("source", "TEXT", false),
                ("created_at", "INTEGER", false),
                ("updated_at", "INTEGER", false),
                ("detached_at", "INTEGER", false),
                ("detached_reason", "TEXT", false),
            ],
            columns![
                ("repo_key", "TEXT", true),
                ("branch", "TEXT", true),
                ("provider", "TEXT", true),
                ("project", "TEXT", true),
                ("external_id", "TEXT", true),
                ("task_id", "INTEGER", false),
                ("issue_number", "INTEGER", false),
                ("status", "TEXT", false),
                ("source", "TEXT", false),
                ("created_at", "INTEGER", false),
                ("updated_at", "INTEGER", false),
                ("detached_at", "INTEGER", false),
                ("detached_reason", "TEXT", false),
            ],
        ],
        &["repo_key", "branch", "provider", "project", "external_id"],
    ),
    (
        "checkout_baselines",
        &[columns![
            ("repo_key", "TEXT", true),
            ("provider", "TEXT", true),
            ("project", "TEXT", true),
            ("issue_number", "INTEGER", true),
            ("checkout_path", "TEXT", true),
            ("issue_branch", "TEXT", false),
            ("baseline_branch", "TEXT", false),
            ("baseline_head", "TEXT", false),
            ("clean_evidence", "TEXT", false),
            ("default_branch", "TEXT", false),
            ("recorded_at", "INTEGER", false),
            ("state", "TEXT", false),
            ("update_state", "TEXT", false),
            ("restored_at", "INTEGER", false),
        ]],
        &[
            "repo_key",
            "provider",
            "project",
            "issue_number",
            "checkout_path",
        ],
    ),
    (
        "worktree_registry_meta",
        &[columns![("name", "TEXT", true), ("value", "TEXT", false)]],
        &["name", "value"],
    ),
    (
        "worktree_directories",
        &[columns![
            ("directory_id", "TEXT", true),
            ("repo_identity", "TEXT", false),
            ("checkout_path", "TEXT", false),
            ("worktree_path", "TEXT", false),
            ("branch", "TEXT", false),
            ("created_at", "INTEGER", false),
        ]],
        &[
            "directory_id",
            "repo_identity",
            "checkout_path",
            "worktree_path",
        ],
    ),
    (
        "worktree_directory_tasks",
        &[columns![
            ("directory_id", "TEXT", true),
            ("task_id", "INTEGER", true),
            ("issue", "INTEGER", false),
            ("created_at", "INTEGER", false),
        ]],
        &["directory_id", "task_id"],
    ),
    (
        "worktree_directory_sessions",
        &[columns![
            ("directory_id", "TEXT", true),
            ("session", "TEXT", true),
            ("created_at", "INTEGER", false),
        ]],
        &["directory_id", "session"],
    ),
    (
        "worktree_directory_legacy_issues",
        &[columns![
            ("directory_id", "TEXT", true),
            ("issue", "INTEGER", true),
            ("provider", "TEXT", true),
            ("project", "TEXT", true),
            ("external_id", "INTEGER", true),
            ("created_at", "INTEGER", false),
        ]],
        &[
            "directory_id",
            "issue",
            "provider",
            "project",
            "external_id",
        ],
    ),
];

/// The known phasegent table bundles.
///
/// A source is only accepted when it carries at least one whole bundle with
/// the known shapes. The bundles are the table groups phasegent creates
/// together, so a database that merely happens to own a table with a
/// phasegent-like name is not a phasegent database.
const KNOWN_BUNDLES: &[&[&str]] = &[
    &[
        "role_config",
        "role_redmine_config",
        "role_credential",
        "global_setting",
    ],
    &["execution_timer_runs", "notification_deliveries"],
    &["task_bindings", "task_repo_memberships"],
    &[
        "worktree_registry_meta",
        "worktree_directories",
        "worktree_directory_tasks",
        "worktree_directory_sessions",
    ],
    &["branch_issue_links"],
    &["checkout_baselines"],
];

/// Row counts of the seven legacy configuration tables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableCounts {
    pub counts: Vec<(&'static str, usize)>,
}

impl TableCounts {
    pub fn total(&self) -> usize {
        self.counts.iter().map(|(_, count)| count).sum()
    }
}

/// A source database held for the whole migration.
///
/// The write transaction keeps every other writer out for the entire
/// export, verification, backup, and purge sequence, so the exported rows,
/// the backup, and the purge all describe one consistent snapshot. The
/// separate read-only connection is the source of the online backup.
pub struct LockedSource {
    connection: Connection,
    reader: Connection,
}

impl LockedSource {
    /// The transaction that holds the source stable.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The read-only connection used for the online backup.
    pub fn reader(&self) -> &Connection {
        &self.reader
    }

    /// Roll the held transaction back, so a failed migration leaves the
    /// source exactly as it was.
    pub fn rollback(&mut self) -> Result<(), String> {
        self.connection
            .execute_batch("ROLLBACK")
            .map_err(|error| format!("could not roll the source transaction back: {error}"))
    }

    /// Clear the configuration rows of the held source and commit.
    ///
    /// The rows are re-read inside the held transaction and compared with
    /// the exported rows first, so a source that changed in the meantime is
    /// refused instead of being cleared. `verify` runs after the deletes
    /// and immediately before the commit, inside the same transaction: a
    /// failed verification rolls every delete back, so the source keeps
    /// its configuration rows.
    pub fn purge(
        &mut self,
        expected: &LegacyConfigRows,
        verify: impl FnOnce() -> Result<(), String>,
    ) -> Result<TableCounts, String> {
        purge_in_transaction(&mut self.connection, expected, verify)
    }
}

/// Open the source and hold it for the duration of `action`.
///
/// The write transaction is started immediately, so no other process can
/// change the configuration between the export and the purge. The caller
/// keeps the configuration file locked for the same span.
pub fn with_locked_source<R>(
    path: &Path,
    action: impl FnOnce(&mut LockedSource) -> Result<R, String>,
) -> Result<R, String> {
    if !path.exists() {
        return Err(format!(
            "the source database at {} does not exist",
            path.display()
        ));
    }
    let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("could not open the source database read-only: {error}"))?;
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|error| {
            format!("could not open the source database for the migration: {error}")
        })?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| {
            format!("could not hold the source database for the migration: {error}")
        })?;
    let mut locked = LockedSource { connection, reader };
    action(&mut locked)
}

/// Read the seven legacy configuration tables. A table that does not exist
/// contributes no rows; a table with an unexpected column is refused.
///
/// Every read is checked against the row count of its own table, so a
/// table that exists but was not read completely — for example because
/// its name is stored in another case — is refused instead of being
/// reported as a successfully migrated table.
pub fn read_rows(connection: &Connection) -> Result<LegacyConfigRows, String> {
    validate_shape(connection)?;
    let rows = LegacyConfigRows {
        role_configs: read_role_configs(connection)?,
        redmine_configs: read_redmine_configs(connection)?,
        forgejo_configs: read_forgejo_configs(connection)?,
        gitlab_configs: read_gitlab_configs(connection)?,
        credentials: read_credentials(connection)?,
        redmine_users: read_redmine_users(connection)?,
        global_settings: read_global_settings(connection)?,
        toml_overlay: None,
    };
    verify_reads(connection, &rows)?;
    Ok(rows)
}

/// Whether every configuration table was read completely.
///
/// The comparison uses the same case-insensitive table identity as every
/// other operation, so a table stored as `ROLE_CREDENTIAL` is compared
/// against its own rows and can never be silently omitted.
fn verify_reads(connection: &Connection, rows: &LegacyConfigRows) -> Result<(), String> {
    let reads: [(&str, usize); 7] = [
        ("role_config", rows.role_configs.len()),
        ("role_redmine_config", rows.redmine_configs.len()),
        ("role_forgejo_config", rows.forgejo_configs.len()),
        ("role_gitlab_config", rows.gitlab_configs.len()),
        ("role_credential", rows.credentials.len()),
        ("role_redmine_user", rows.redmine_users.len()),
        ("global_setting", rows.global_settings.len()),
    ];
    for (table, read) in reads {
        if !table_exists(connection, table)? {
            continue;
        }
        let stored = row_count(connection, table)?;
        if stored != read {
            return Err(format!(
                "the source table {table} could not be read completely; \
                 refusing to migrate it"
            ));
        }
    }
    Ok(())
}

/// Count the rows of every legacy configuration table.
pub fn count_rows(connection: &Connection) -> Result<TableCounts, String> {
    let mut counts = Vec::with_capacity(CONFIG_TABLES.len());
    for table in CONFIG_TABLES {
        if !table_exists(connection, table)? {
            counts.push((table, 0));
            continue;
        }
        counts.push((table, row_count(connection, table)?));
    }
    Ok(TableCounts { counts })
}

/// The number of rows a table holds.
fn row_count(connection: &Connection, table: &str) -> Result<usize, String> {
    let count: i64 = connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("could not count the rows of {table}: {error}"))?;
    Ok(count.max(0) as usize)
}

/// Reserve a private backup file next to the configuration file.
///
/// The file is created exclusively with owner-only permissions before
/// SQLite ever opens it, so an existing database, a symlink, or a leftover
/// file with the same name is never overwritten. The name carries
/// nanosecond resolution, the process id, and a counter, so a repeated run
/// never collides with an earlier backup.
pub fn create_backup_file(parent: &Path) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..16u64 {
        let candidate = parent.join(format!(
            "phasegent-config-backup-{stamp}-{}-{attempt}.sqlite3",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).read(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => {
                drop(file);
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "could not create the backup file at {}: {error}",
                    candidate.display()
                ));
            }
        }
    }
    Err("could not create a unique backup file next to the configuration file".to_owned())
}

/// Make a consistent private backup of the source database. The SQLite
/// online backup API is used, so a WAL database is never copied file by
/// file and the copy is always a consistent snapshot.
pub fn backup(source: &Connection, destination: &Path) -> Result<(), String> {
    ensure_regular_file(destination)?;
    let mut target = Connection::open(destination)
        .map_err(|error| format!("could not open the backup file for writing: {error}"))?;
    {
        let backup = rusqlite::backup::Backup::new(source, &mut target)
            .map_err(|error| format!("could not start the database backup: {error}"))?;
        backup
            .run_to_completion(64, std::time::Duration::from_millis(0), None)
            .map_err(|error| format!("could not write the database backup: {error}"))?;
    }
    target
        .close()
        .map_err(|(_, error)| format!("could not close the database backup: {error}"))?;
    make_private(destination)?;
    Ok(())
}

/// Refuse a backup target that is not a plain regular file, so a symlink
/// can never redirect the backup to another database.
fn ensure_regular_file(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "the backup path at {} must be a regular non-symlink file",
                    path.display()
                ));
            }
            Ok(())
        }
        Err(error) => Err(format!(
            "could not inspect the backup file at {}: {error}",
            path.display()
        )),
    }
}

/// Make the backup file owner-only on Unix.
fn make_private(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                format!(
                    "could not set permissions on the backup file at {}: {error}",
                    path.display()
                )
            },
        )?;
    }
    Ok(())
}

/// Clear the rows of the seven legacy configuration tables in a single
/// transaction, but only when the source rows still match what was
/// exported. A source that changed in the meantime is refused so the
/// purge can never remove values that were never migrated. The database
/// file and every business row stay untouched.
///
/// `verify` runs after the deletes and immediately before the commit,
/// inside the same transaction. A failed verification rolls every delete
/// back, so a destination that changed or disappeared while the purge ran
/// leaves the source configuration untouched.
pub fn purge_in_transaction(
    connection: &mut Connection,
    expected: &LegacyConfigRows,
    verify: impl FnOnce() -> Result<(), String>,
) -> Result<TableCounts, String> {
    let current = read_rows(connection)?;
    if current != without_overlay(expected) {
        let _ = connection.execute_batch("ROLLBACK");
        return Err(
            "the source configuration changed while the migration was running; \
             refusing to clear it"
                .to_owned(),
        );
    }
    let mut counts = Vec::with_capacity(CONFIG_TABLES.len());
    for table in CONFIG_TABLES {
        if !table_exists(connection, table)? {
            counts.push((table, 0));
            continue;
        }
        connection
            .execute(&format!("DELETE FROM {table}"), [])
            .map_err(|error| format!("could not clear the rows of {table}: {error}"))?;
        let cleared = connection.changes();
        counts.push((table, cleared as usize));
    }
    // The destination is checked once more inside the held transaction,
    // after the deletes and immediately before the commit. A mismatch
    // rolls every delete back.
    if let Err(error) = verify() {
        let _ = connection.execute_batch("ROLLBACK");
        return Err(error);
    }
    connection
        .execute_batch("COMMIT")
        .map_err(|error| format!("could not commit the configuration purge: {error}"))?;
    Ok(TableCounts { counts })
}

/// The exported rows without the TOML overlay, so the comparison against
/// the re-read source only covers the database values.
fn without_overlay(rows: &LegacyConfigRows) -> LegacyConfigRows {
    LegacyConfigRows {
        role_configs: rows.role_configs.clone(),
        forgejo_configs: rows.forgejo_configs.clone(),
        redmine_configs: rows.redmine_configs.clone(),
        gitlab_configs: rows.gitlab_configs.clone(),
        credentials: rows.credentials.clone(),
        redmine_users: rows.redmine_users.clone(),
        global_settings: rows.global_settings.clone(),
        toml_overlay: None,
    }
}

/// Whether the source owns a table with this name.
///
/// SQLite resolves identifiers case-insensitively, so a table stored as
/// `ROLE_CREDENTIAL` is the same table as `role_credential`. The
/// comparison therefore ignores case, and every other table operation
/// (read, count, purge, trigger and foreign-key filter) uses the same
/// rule, so no configuration table can be skipped because of the case of
/// its name.
fn table_exists(connection: &Connection, table: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master \
             WHERE type = 'table' AND name = ?1 COLLATE NOCASE)",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found == 1)
        .map_err(|error| format!("could not inspect the source schema: {error}"))
}

/// Refuse a source that is not corroborated as a known phasegent
/// configuration store.
///
/// The check has four parts: every configuration table that is present must
/// match one of its known schema variants exactly, every known business
/// table that is present must match a known variant, the source must carry
/// at least one whole known phasegent table bundle, and the configuration
/// tables must not carry a trigger or a foreign key that could reach
/// beyond the cleared rows.
fn validate_shape(connection: &Connection) -> Result<(), String> {
    let mut configuration_tables = 0;
    for (table, variants) in CONFIG_SHAPES {
        if !table_exists(connection, table)? {
            continue;
        }
        configuration_tables += 1;
        let actual = columns_of(connection, table)?;
        if !variants
            .iter()
            .any(|variant| matches_exactly(variant, &actual))
        {
            return Err(format!(
                "the source table {table} does not match a known phasegent configuration schema; \
                 migrate from a known phasegent configuration database"
            ));
        }
    }
    if configuration_tables == 0 {
        return Err(
            "the source database holds no known phasegent configuration table; \
             refusing to migrate it"
                .to_owned(),
        );
    }
    for (table, variants, required) in BUSINESS_SHAPES {
        if !table_exists(connection, table)? {
            continue;
        }
        let actual = columns_of(connection, table)?;
        let known = variants.iter().any(|variant| {
            matches_subset(variant, &actual)
                && required.iter().all(|name| {
                    actual
                        .iter()
                        .any(|column| column.name.eq_ignore_ascii_case(name))
                })
        });
        if !known {
            return Err(format!(
                "the source table {table} does not match a known phasegent table shape; \
                 migrate from a known phasegent configuration database"
            ));
        }
    }
    if !has_known_bundle(connection)? {
        return Err(
            "the source database does not carry a known phasegent table bundle; \
             refusing to migrate it"
                .to_owned(),
        );
    }
    reject_dangerous_schema(connection)
}

/// Whether the source carries one of the known phasegent table bundles.
fn has_known_bundle(connection: &Connection) -> Result<bool, String> {
    let present = all_tables(connection)?;
    Ok(KNOWN_BUNDLES
        .iter()
        .any(|bundle| bundle_is_complete(bundle, &present)))
}

/// Whether every table of a bundle is present. The comparison ignores
/// case, because SQLite resolves table names that way.
fn bundle_is_complete(bundle: &[&str], present: &[String]) -> bool {
    bundle
        .iter()
        .all(|table| present.iter().any(|name| name.eq_ignore_ascii_case(table)))
}

/// Refuse triggers on the configuration tables and any foreign key that
/// could cascade into or out of them.
fn reject_dangerous_schema(connection: &Connection) -> Result<(), String> {
    // The trigger owner is compared with the same case-insensitive rule
    // every other table operation uses, so a trigger on a table stored
    // as `ROLE_CREDENTIAL` is still found.
    let mut statement = connection
        .prepare("SELECT tbl_name FROM sqlite_master WHERE type = 'trigger'")
        .map_err(|error| format!("could not inspect the source triggers: {error}"))?;
    let triggers = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("could not read the source triggers: {error}"))?;
    for trigger in triggers {
        let table =
            trigger.map_err(|error| format!("could not read the source triggers: {error}"))?;
        if is_config_table(&table) {
            return Err(
                "the source configuration tables carry a trigger; refusing to clear them"
                    .to_owned(),
            );
        }
    }
    for table in CONFIG_TABLES {
        if !table_exists(connection, table)? {
            continue;
        }
        if foreign_keys_of(connection, table)?
            .iter()
            .any(|target| is_config_table(target))
        {
            return Err(format!(
                "the source table {table} carries a foreign key; refusing to clear it"
            ));
        }
    }
    for table in all_tables(connection)? {
        for target in foreign_keys_of(connection, &table)? {
            // SQLite resolves table names case-insensitively, so a
            // reference to `GLOBAL_SETTING` reaches the same rows as a
            // reference to `global_setting`.
            if is_config_table(&target) {
                return Err(format!(
                    "the source table {table} references a configuration table; \
                     refusing to clear the configuration rows"
                ));
            }
        }
    }
    Ok(())
}

/// Whether a foreign-key target names a configuration table. The
/// comparison ignores case, because SQLite resolves table names that way.
fn is_config_table(target: &str) -> bool {
    CONFIG_TABLES
        .iter()
        .any(|table| table.eq_ignore_ascii_case(target))
}

/// The tables the source owns.
fn all_tables(connection: &Connection) -> Result<Vec<String>, String> {
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .map_err(|error| format!("could not inspect the source tables: {error}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("could not read the source tables: {error}"))?;
    let mut tables = Vec::new();
    for table in rows {
        tables.push(table.map_err(|error| format!("could not read the source tables: {error}"))?);
    }
    Ok(tables)
}

/// The tables a table references through a foreign key.
fn foreign_keys_of(connection: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA foreign_key_list({table})"))
        .map_err(|error| format!("could not inspect the foreign keys of {table}: {error}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(2))
        .map_err(|error| format!("could not read the foreign keys of {table}: {error}"))?;
    let mut targets = Vec::new();
    for target in rows {
        targets.push(
            target
                .map_err(|error| format!("could not read the foreign keys of {table}: {error}"))?,
        );
    }
    Ok(targets)
}

/// The columns of a table, in declaration order, with their declared type
/// and their place in the primary key.
fn columns_of(connection: &Connection, table: &str) -> Result<Vec<ColumnInfo>, String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("could not inspect the columns of {table}: {error}"))?;
    let mut rows = statement
        .query([])
        .map_err(|error| format!("could not read the columns of {table}: {error}"))?;
    let mut actual = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| format!("could not advance the columns of {table}: {error}"))?
    {
        let name: String = row
            .get(1)
            .map_err(|error| format!("could not read a column name of {table}: {error}"))?;
        let ty: String = row
            .get(2)
            .map_err(|error| format!("could not read a column type of {table}: {error}"))?;
        let primary_key: i64 = row
            .get(5)
            .map_err(|error| format!("could not read a column key of {table}: {error}"))?;
        actual.push(ColumnInfo {
            name,
            ty,
            primary_key: primary_key > 0,
        });
    }
    Ok(actual)
}

/// Whether the actual columns are exactly the known variant: the same
/// columns, with the same declared types and the same primary key.
fn matches_exactly(variant: &[KnownColumn], actual: &[ColumnInfo]) -> bool {
    variant.len() == actual.len()
        && actual
            .iter()
            .all(|column| match known_column(variant, &column.name) {
                Some(known) => {
                    known.ty.eq_ignore_ascii_case(&column.ty)
                        && known.primary_key == column.primary_key
                }
                None => false,
            })
}

/// Whether the actual columns are an older subset of the known variant:
/// every actual column is known, with the same declared type and the same
/// primary key.
fn matches_subset(variant: &[KnownColumn], actual: &[ColumnInfo]) -> bool {
    actual
        .iter()
        .all(|column| match known_column(variant, &column.name) {
            Some(known) => {
                known.ty.eq_ignore_ascii_case(&column.ty) && known.primary_key == column.primary_key
            }
            None => false,
        })
}

/// The known column of a variant with the given name.
fn known_column<'a>(variant: &'a [KnownColumn], name: &str) -> Option<&'a KnownColumn> {
    variant
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(name))
}

/// The SQL expression for a column that a known older schema may not
/// carry: an absent nullable column reads as NULL instead of failing the
/// whole read.
fn optional_column(connection: &Connection, table: &str, column: &str) -> Result<String, String> {
    let columns = columns_of(connection, table)?;
    if columns
        .iter()
        .any(|known| known.name.eq_ignore_ascii_case(column))
    {
        Ok(column.to_owned())
    } else {
        Ok("NULL".to_owned())
    }
}

fn read_role_configs(connection: &Connection) -> Result<Vec<LegacyRoleConfigRow>, String> {
    if !table_exists(connection, "role_config")? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare("SELECT role, provider, api_base, repository FROM role_config ORDER BY role")
        .map_err(|error| format!("could not prepare the role config read: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(LegacyRoleConfigRow {
                role: row.get(0)?,
                provider: row.get(1)?,
                api_base: row.get(2)?,
                repository: row.get(3)?,
            })
        })
        .map_err(|error| format!("could not read the role config rows: {error}"))?;
    collect(rows, "role config")
}

fn read_redmine_configs(connection: &Connection) -> Result<Vec<LegacyRedmineConfigRow>, String> {
    if !table_exists(connection, "role_redmine_config")? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT role, api_base, project_id, close_status_id \
             FROM role_redmine_config ORDER BY role",
        )
        .map_err(|error| format!("could not prepare the Redmine config read: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .map_err(|error| format!("could not read the Redmine config rows: {error}"))?;
    let mut collected = Vec::new();
    for row in rows {
        let (role, api_base, project_id, close_status_id) =
            row.map_err(|error| format!("could not read the Redmine config rows: {error}"))?;
        let close_status_id = match close_status_id {
            // The model cannot represent a negative close status id, and
            // the legacy runtime ignored such a value, so the row is
            // refused instead of being migrated as a changed number.
            Some(value) if value < 0 => {
                return Err(
                    "the source holds a Redmine config row with a negative close status id; \
                     refusing to migrate it"
                        .to_owned(),
                );
            }
            Some(value) => Some(value as u64),
            None => None,
        };
        collected.push(LegacyRedmineConfigRow {
            role,
            api_base,
            project_id,
            close_status_id,
        });
    }
    Ok(collected)
}

fn read_forgejo_configs(connection: &Connection) -> Result<Vec<LegacyForgejoConfigRow>, String> {
    if !table_exists(connection, "role_forgejo_config")? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT role, api_base, repository, source, updated_at \
             FROM role_forgejo_config ORDER BY role",
        )
        .map_err(|error| format!("could not prepare the Forgejo config read: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(LegacyForgejoConfigRow {
                role: row.get(0)?,
                api_base: row.get(1)?,
                repository: row.get(2)?,
                source: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })
        .map_err(|error| format!("could not read the Forgejo config rows: {error}"))?;
    collect(rows, "Forgejo config")
}

fn read_gitlab_configs(connection: &Connection) -> Result<Vec<LegacyGitlabConfigRow>, String> {
    if !table_exists(connection, "role_gitlab_config")? {
        return Ok(Vec::new());
    }
    // The provenance columns were added later; a known older store simply
    // does not carry them and the values read as NULL.
    let repository = optional_column(connection, "role_gitlab_config", "repository")?;
    let source = optional_column(connection, "role_gitlab_config", "source")?;
    let updated_at = optional_column(connection, "role_gitlab_config", "updated_at")?;
    let mut statement = connection
        .prepare(&format!(
            "SELECT role, api_base, project_id, {repository}, {source}, {updated_at} \
             FROM role_gitlab_config ORDER BY role"
        ))
        .map_err(|error| format!("could not prepare the GitLab config read: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(LegacyGitlabConfigRow {
                role: row.get(0)?,
                api_base: row.get(1)?,
                project_id: row.get(2)?,
                repository: row.get(3)?,
                source: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })
        .map_err(|error| format!("could not read the GitLab config rows: {error}"))?;
    collect(rows, "GitLab config")
}

fn read_credentials(connection: &Connection) -> Result<Vec<LegacyCredentialRow>, String> {
    if !table_exists(connection, "role_credential")? {
        return Ok(Vec::new());
    }
    // The timestamp column was added later; a known older store does not
    // carry it and the value reads as NULL.
    let updated_at = optional_column(connection, "role_credential", "credential_updated_at")?;
    let mut statement = connection
        .prepare(&format!(
            "SELECT role, provider, credential, {updated_at} \
             FROM role_credential ORDER BY role, provider"
        ))
        .map_err(|error| format!("could not prepare the credential read: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(LegacyCredentialRow {
                role: row.get(0)?,
                provider: row.get(1)?,
                credential: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })
        .map_err(|error| format!("could not read the credential rows: {error}"))?;
    collect(rows, "credential")
}

fn read_redmine_users(connection: &Connection) -> Result<Vec<LegacyRedmineUserRow>, String> {
    if !table_exists(connection, "role_redmine_user")? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare("SELECT role, user_id, login FROM role_redmine_user ORDER BY role")
        .map_err(|error| format!("could not prepare the Redmine user read: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| format!("could not read the Redmine user rows: {error}"))?;
    let mut collected = Vec::new();
    for row in rows {
        let (role, user_id, login) =
            row.map_err(|error| format!("could not read the Redmine user rows: {error}"))?;
        // The stored id must be a positive number: the model cannot
        // represent a negative one, and the legacy runtime ignored such a
        // row, so it is refused instead of being migrated as a changed
        // number.
        if user_id <= 0 {
            return Err(
                "the source holds a Redmine user row with a non-positive user id; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
        collected.push(LegacyRedmineUserRow {
            role,
            user_id: user_id as u64,
            login,
        });
    }
    Ok(collected)
}

fn read_global_settings(connection: &Connection) -> Result<Vec<LegacyGlobalSettingRow>, String> {
    if !table_exists(connection, "global_setting")? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare("SELECT name, value FROM global_setting ORDER BY name")
        .map_err(|error| format!("could not prepare the global setting read: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(LegacyGlobalSettingRow {
                name: row.get(0)?,
                value: row.get(1)?,
            })
        })
        .map_err(|error| format!("could not read the global setting rows: {error}"))?;
    collect(rows, "global setting")
}

fn collect<T>(
    rows: impl Iterator<Item = rusqlite::Result<T>>,
    label: &str,
) -> Result<Vec<T>, String> {
    let mut collected = Vec::new();
    for row in rows {
        collected.push(row.map_err(|error| format!("could not read the {label} rows: {error}"))?);
    }
    Ok(collected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, ty: &str, primary_key: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.to_owned(),
            ty: ty.to_owned(),
            primary_key,
        }
    }

    fn config_shape(table: &str) -> &'static [&'static [KnownColumn]] {
        CONFIG_SHAPES
            .iter()
            .find(|(name, _)| *name == table)
            .map(|(_, variants)| *variants)
            .expect("the configuration table has known variants")
    }

    fn business_shape(table: &str) -> &'static [&'static [KnownColumn]] {
        BUSINESS_SHAPES
            .iter()
            .find(|(name, _, _)| *name == table)
            .map(|(_, variants, _)| *variants)
            .expect("the business table has known variants")
    }

    #[test]
    fn the_historical_three_column_credential_table_is_known() {
        let variants = config_shape("role_credential");
        let historical = vec![
            column("role", "TEXT", true),
            column("provider", "TEXT", true),
            column("credential", "TEXT", false),
        ];
        assert!(
            variants
                .iter()
                .any(|variant| matches_exactly(variant, &historical))
        );

        let with_fingerprint = vec![
            column("role", "TEXT", true),
            column("provider", "TEXT", true),
            column("credential", "TEXT", false),
            column("fingerprint", "TEXT", false),
        ];
        assert!(
            variants
                .iter()
                .any(|variant| matches_exactly(variant, &with_fingerprint))
        );

        let current = vec![
            column("role", "TEXT", true),
            column("provider", "TEXT", true),
            column("credential", "TEXT", false),
            column("fingerprint", "TEXT", false),
            column("credential_updated_at", "INTEGER", false),
        ];
        assert!(
            variants
                .iter()
                .any(|variant| matches_exactly(variant, &current))
        );
    }

    #[test]
    fn an_unknown_column_shape_is_refused() {
        let variants = config_shape("global_setting");
        let generic = vec![
            column("id", "INTEGER", true),
            column("value", "TEXT", false),
        ];
        assert!(
            !variants
                .iter()
                .any(|variant| matches_exactly(variant, &generic)),
            "a generic table must not pass as a phasegent table"
        );
        let wrong_type = vec![
            column("name", "TEXT", true),
            column("value", "INTEGER", false),
        ];
        assert!(
            !variants
                .iter()
                .any(|variant| matches_exactly(variant, &wrong_type)),
            "a wrong column type must not pass"
        );
        let wrong_key = vec![
            column("name", "TEXT", false),
            column("value", "TEXT", false),
        ];
        assert!(
            !variants
                .iter()
                .any(|variant| matches_exactly(variant, &wrong_key)),
            "a wrong primary key must not pass"
        );
    }

    #[test]
    fn an_older_business_table_may_be_a_subset_of_a_known_variant() {
        let variants = business_shape("execution_timer_runs");
        let older = vec![
            column("run_id", "TEXT", true),
            column("issue_id", "INTEGER", false),
            column("phase", "TEXT", false),
            column("role", "TEXT", false),
            column("attempt", "INTEGER", false),
            column("started_at", "INTEGER", false),
            column("status", "TEXT", false),
        ];
        assert!(
            variants
                .iter()
                .any(|variant| matches_subset(variant, &older)),
            "an older ledger shape must stay readable"
        );
        let unrelated = vec![column("id", "INTEGER", true)];
        assert!(
            !variants
                .iter()
                .any(|variant| matches_subset(variant, &unrelated)),
            "a generic table must not pass as a phasegent ledger"
        );
    }

    #[test]
    fn a_foreign_key_target_is_matched_case_insensitively() {
        assert!(is_config_table("global_setting"));
        assert!(is_config_table("GLOBAL_SETTING"));
        assert!(is_config_table("Global_Setting"));
        assert!(!is_config_table("business_rows"));
    }

    #[test]
    fn a_bundle_needs_every_one_of_its_tables() {
        let complete = vec![
            "execution_timer_runs".to_owned(),
            "notification_deliveries".to_owned(),
        ];
        assert!(bundle_is_complete(
            &["execution_timer_runs", "notification_deliveries"],
            &complete
        ));
        assert!(!bundle_is_complete(
            &["execution_timer_runs", "notification_deliveries"],
            &["execution_timer_runs".to_owned()]
        ));
        assert!(bundle_is_complete(
            &["execution_timer_runs", "notification_deliveries"],
            &[
                "EXECUTION_TIMER_RUNS".to_owned(),
                "notification_deliveries".to_owned()
            ]
        ));
    }

    #[test]
    fn the_worktree_task_shape_matches_the_current_registry_ddl() {
        // `src/worktree/registry_schema.rs` declares `task_id INTEGER`,
        // `issue INTEGER` and `created_at INTEGER`.
        let variants = business_shape("worktree_directory_tasks");
        let current = vec![
            column("directory_id", "TEXT", true),
            column("task_id", "INTEGER", true),
            column("issue", "INTEGER", false),
            column("created_at", "INTEGER", false),
        ];
        assert!(
            variants
                .iter()
                .any(|variant| matches_subset(variant, &current)),
            "the current worktree registry shape must stay a known business shape"
        );
    }

    #[test]
    fn every_configuration_table_has_a_known_variant() {
        for table in CONFIG_TABLES {
            assert!(
                !config_shape(table).is_empty(),
                "{table} has no known schema variant"
            );
        }
    }
}
