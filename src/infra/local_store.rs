//! Local provider storage backends.
//!
//! SQLite file `phasegent-local.sqlite3` is independent from config and
//! index files. Backend selection is URL-driven via the same
//! `PHASEGENT_INDEX_PG_URL` as the index (env overrides persisted global):
//! non-empty URL selects PostgreSQL, absent/blank selects SQLite. Only one
//! backend is ever active (single-active, no dual-write). Static SQL lives
//! in `.sql` files (`local_sql/*.sql` for SQLite, `migrations/pg/0002_*`
//! for PostgreSQL) and is embedded via `include_str!` with runtime
//! `rusqlite`/`sqlx::query` APIs; compile-time `query_file!` is avoided
//! for the same reason as `issue_index_postgres.rs` (no offline query
//! metadata/live DB for `cargo check`).

use crate::infra::issue_index_backend::{IndexBackendKind, resolve_index_backend};
use crate::infra::local_schema::{
    DB_FILENAME_LOCAL, PRAGMA_STATEMENTS_LOCAL, SCHEMA_LOCAL, SEED_LOCAL,
};
use crate::infra::storage::Storage;
use directories::ProjectDirs;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "postgres")]
use sqlx::postgres::PgPoolOptions;
#[cfg(feature = "postgres")]
use sqlx::PgPool;

/// SQLite backend for the local provider. Stands up the connection,
/// schema, and seeds.
#[allow(dead_code)]
pub struct SqliteLocalStore {
    pub(crate) connection: Connection,
    path: PathBuf,
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
            create_private_dir(parent)?;
        }
        let connection = Connection::open(path)
            .map_err(|e| format!("could not open local database: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta =
                fs::metadata(path).map_err(|e| format!("could not stat local database: {e}"))?;
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(path, perms)
                .map_err(|e| format!("could not secure local database: {e}"))?;
        }
        Self::initialise(&connection)?;
        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
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

    pub fn db_path(&self) -> &Path {
        &self.path
    }
}

/// Exact-name opener: independent file plus
/// `CREATE TABLE IF NOT EXISTS` via [`SqliteLocalStore::open`].
#[allow(dead_code)]
pub fn open_local() -> Result<SqliteLocalStore, String> {
    SqliteLocalStore::open()
}

/// Testable exact-name opener at an explicit path.
#[allow(dead_code)]
pub fn open_local_at(path: &Path) -> Result<SqliteLocalStore, String> {
    SqliteLocalStore::open_at(path)
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    let existed = path.exists();
    fs::create_dir_all(path).map_err(|e| format!("could not create local directory: {e}"))?;
    #[cfg(unix)]
    {
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("could not secure local directory: {e}"))?;
        }
    }
    Ok(())
}

fn project_dirs_local_path() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("com", "Cloud1ful", "phasegent")
        .ok_or_else(|| "could not resolve phasegent config directory".to_owned())?;
    Ok(dirs.config_dir().join(DB_FILENAME_LOCAL))
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
const PG_MIGRATION_SQL: &str = include_str!("../../migrations/pg/0002_local.sql");

#[cfg(feature = "postgres")]
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

#[cfg(not(feature = "postgres"))]
#[allow(dead_code)]
pub struct PostgresLocalStore;

#[cfg(not(feature = "postgres"))]
impl PostgresLocalStore {
    pub async fn open(_url: &str) -> Result<Self, String> {
        Err("postgres local support is not enabled; rebuild with --features postgres".to_owned())
    }
}

/// Single-active dispatcher: exactly one backend per process, never both.
#[allow(dead_code)]
pub enum LocalStore {
    Sqlite(SqliteLocalStore),
    Postgres(PostgresLocalStore),
}

#[allow(dead_code)]
impl LocalStore {
    pub async fn open() -> Result<Self, String> {
        let storage = Storage::open()?;
        Self::open_with_storage(&storage).await
    }

    pub async fn open_with_storage(storage: &Storage) -> Result<Self, String> {
        match resolve_index_backend(storage)? {
            IndexBackendKind::Sqlite => Ok(Self::Sqlite(SqliteLocalStore::open()?)),
            IndexBackendKind::Postgres => {
                let url = crate::infra::issue_index_backend::resolve_pg_url(storage)?.ok_or_else(
                    || {
                        "postgres local backend requires PHASEGENT_INDEX_PG_URL; use config set index-pg-url --stdin"
                            .to_owned()
                    },
                )?;
                Ok(Self::Postgres(PostgresLocalStore::open(&url).await?))
            }
        }
    }

    pub fn kind(&self) -> IndexBackendKind {
        match self {
            Self::Sqlite(_) => IndexBackendKind::Sqlite,
            Self::Postgres(_) => IndexBackendKind::Postgres,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::local_schema::STATUS_TRANSITION_SEED_LEN;

    fn tmp_path(label: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "phasegent-local-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join(DB_FILENAME_LOCAL);
        (dir, path)
    }

    #[test]
    fn open_creates_independent_file_with_schema_and_seeds() {
        let (dir, path) = tmp_path("schema");
        assert!(path.to_str().unwrap().contains("phasegent-local"));
        let store = open_local_at(&path).unwrap();
        assert!(path.exists());
        for table in [
            "local_issues",
            "local_comments",
            "local_projects",
            "status_transitions",
        ] {
            let count: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }
        let edges: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM status_transitions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(edges, STATUS_TRANSITION_SEED_LEN as i64);
        let marker_unique: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='local_comments' AND sql LIKE '%UNIQUE%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marker_unique, 1, "local_comments.marker must be UNIQUE");
        drop(store);
        let reopened = open_local_at(&path).unwrap();
        let edges2: i64 = reopened
            .connection
            .query_row("SELECT COUNT(*) FROM status_transitions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(edges2, edges, "seeds must be idempotent");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn marker_unique_is_enforced() {
        let (dir, path) = tmp_path("marker");
        let store = open_local_at(&path).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO local_issues (title, body, status, project, tracker, author_role, created_at, updated_at) VALUES (?1,'',?2,'default','Task','executor',1,1)",
                rusqlite::params!["t", "New"],
            )
            .unwrap();
        let issue: i64 = store
            .connection
            .query_row("SELECT id FROM local_issues LIMIT 1", [], |r| r.get(0))
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO local_comments (issue_id, role, phase, attempt, marker, body, created_at) VALUES (?1,'executor','plan',1,'m1','b',1)",
                rusqlite::params![issue],
            )
            .unwrap();
        let dup = store.connection.execute(
            "INSERT INTO local_comments (issue_id, role, phase, attempt, marker, body, created_at) VALUES (?1,'executor','plan',1,'m1','b2',2)",
            rusqlite::params![issue],
        );
        assert!(dup.is_err(), "duplicate marker must fail");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn local_store_selects_sqlite_without_pg_url() {
        let _lock = crate::infra::storage::test_support::lock_workflow_tests();
        let (dir, _) = tmp_path("select");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("phasegent.sqlite3");
        let storage = Storage::open_at(&cfg).unwrap();
        storage.delete_global_setting("PHASEGENT_INDEX_PG_URL").unwrap();
        let _g = crate::infra::storage::test_support::EnvGuard::set(
            "PHASEGENT_INDEX_PG_URL",
            "",
        );
        let kind = resolve_index_backend(&storage).unwrap();
        assert_eq!(kind, IndexBackendKind::Sqlite);
        let _ = std::fs::remove_dir_all(dir);
    }
}
