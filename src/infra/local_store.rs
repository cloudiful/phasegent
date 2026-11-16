//! Local provider storage backends.
//!
//! SQLite file `phasegent-local.sqlite3` is independent from config and
//! index files; `PHASEGENT_LOCAL_DB_PATH` overrides the platform directory
//! for operators. A PostgreSQL backend lives behind the `postgres` feature.
//! Static SQL lives in `.sql` files (`local_sql/*.sql` for SQLite,
//! `migrations/pg/0002_*` for PostgreSQL) and is embedded via
//! `include_str!` with runtime `rusqlite`/`sqlx::query` APIs; compile-time
//! `query_file!` is avoided for the same reason as
//! `issue_index_postgres.rs` (no offline query metadata/live DB for
//! `cargo check`).

use crate::infra::local_schema::{
    DB_FILENAME_LOCAL, PRAGMA_STATEMENTS_LOCAL, SCHEMA_LOCAL, SEED_LOCAL,
};
use crate::infra::sqlite_file;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

#[cfg(feature = "postgres")]
use sqlx::PgPool;
#[cfg(feature = "postgres")]
use sqlx::postgres::PgPoolOptions;

/// SQLite backend for the local provider. Stands up the connection,
/// schema, and seeds.
pub struct SqliteLocalStore {
    pub(crate) connection: Connection,
}

impl SqliteLocalStore {
    pub fn open() -> Result<Self, String> {
        if let Some(p) = std::env::var_os("PHASEGENT_LOCAL_DB_PATH") {
            let path = PathBuf::from(p);
            if !path.is_absolute() {
                return Err("PHASEGENT_LOCAL_DB_PATH must be an absolute path".to_owned());
            }
            return Self::open_at(&path);
        }
        let path = project_dirs_local_path()?;
        Self::open_at(&path)
    }

    pub fn open_at(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            sqlite_file::create_private_dir(parent, "local", true)?;
        }
        let connection = sqlite_file::open_private_connection(path, "local")?;
        Self::initialise(&connection)?;
        Ok(Self { connection })
    }

    /// Open an already existing local store read-only.
    ///
    /// No directory is created, no schema is applied, no seed runs and no
    /// write is possible: an absent store is an error, never a creation.
    pub fn open_read_only_at(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Err("the local store is not present".to_owned());
        }
        let connection = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|error| format!("could not open the local store read-only: {error}"))?;
        Ok(Self { connection })
    }

    fn initialise(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(PRAGMA_STATEMENTS_LOCAL)
            .map_err(|e| format!("could not configure local database: {e}"))?;
        conn.execute_batch(SCHEMA_LOCAL)
            .map_err(|e| format!("could not initialise local schema: {e}"))?;
        conn.execute_batch(SEED_LOCAL)
            .map_err(|e| format!("could not seed local database: {e}"))?;
        Ok(())
    }
}

/// Exact-name opener: independent file plus
/// `CREATE TABLE IF NOT EXISTS` via [`SqliteLocalStore::open`].
pub fn open_local() -> Result<SqliteLocalStore, String> {
    SqliteLocalStore::open()
}

/// Read-only opener for the exact-name store: an existing file only, never
/// created, migrated or written.
pub fn open_local_read_only() -> Result<SqliteLocalStore, String> {
    if let Some(p) = std::env::var_os("PHASEGENT_LOCAL_DB_PATH") {
        let path = PathBuf::from(p);
        if !path.is_absolute() {
            return Err("PHASEGENT_LOCAL_DB_PATH must be an absolute path".to_owned());
        }
        return SqliteLocalStore::open_read_only_at(&path);
    }
    let path = project_dirs_local_path()?;
    SqliteLocalStore::open_read_only_at(&path)
}

fn project_dirs_local_path() -> Result<PathBuf, String> {
    sqlite_file::project_dirs_config_path(DB_FILENAME_LOCAL)
}

/// PostgreSQL backend for the local provider (single-active alternative).
#[cfg(feature = "postgres")]
#[allow(dead_code)]
pub struct PostgresLocalStore {
    #[allow(dead_code)]
    pool: PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresLocalStore {
    #[allow(dead_code)]
    pub async fn open(url: &str) -> Result<Self, String> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(url)
            .await
            .map_err(|_| "could not connect to PostgreSQL local database".to_owned())?;
        apply_embedded_migrations(&pool)
            .await
            .map_err(|_| "could not migrate PostgreSQL local database".to_owned())?;
        Ok(Self { pool })
    }
}

#[cfg(feature = "postgres")]
#[allow(dead_code)]
const PG_MIGRATION_SQL: &str = include_str!("../../migrations/pg/0002_local.sql");

#[cfg(feature = "postgres")]
#[allow(dead_code)]
async fn apply_embedded_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS _local_migrations \
         (version BIGINT PRIMARY KEY, description TEXT NOT NULL, applied_at BIGINT NOT NULL)",
    )
    .execute(pool)
    .await?;
    let applied: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _local_migrations WHERE version=$1")
            .bind(2_i64)
            .fetch_optional(pool)
            .await?;
    if applied.is_some() {
        return Ok(());
    }
    sqlx::raw_sql(PG_MIGRATION_SQL).execute(pool).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_700_000_000);
    sqlx::query(
        "INSERT INTO _local_migrations (version, description, applied_at) \
         VALUES ($1,$2,$3) ON CONFLICT (version) DO NOTHING",
    )
    .bind(2_i64)
    .bind("local")
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}
