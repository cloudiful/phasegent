//! Lease-table DDL and additive migrations (issue 628 P4).
//!
//! Split from `leases.rs` so schema text and index migration live in one
//! cohesive module. `leases.rs` keeps query/insert helpers and calls
//! [`ensure_lease_schema`] from its own `ensure_schema`.

use crate::infra::storage::Storage;

/// Full lease-table DDL for fresh databases: active-only path
/// uniqueness plus the repo/status lookup index. Pre-P4 databases
/// carry the old full-table `worktree_leases_repo_path_idx`, which
/// [`ensure_active_path_index`] drops on open.
pub(super) const WORKTREE_LEASES_SCHEMA: &str = "\
-- `release_reason` records the operator justification for a forced
-- release (`worktree release --force --reason`); ordinary releases
-- keep it NULL. Lease rows are audit records and are never deleted.
--
-- The path uniqueness is active-only since issue 628 P4: at most one
-- `active` lease per `(repo_identity, worktree_path)` (partial index
-- below), while `retained` / `released` rows stay as history and never
-- block a later active lease on the same checkout. Databases created
-- before P4 carry the old full-table `worktree_leases_repo_path_idx`,
-- which `ensure_active_path_index` drops on open.
CREATE TABLE IF NOT EXISTS worktree_leases (
    lease_id TEXT PRIMARY KEY,
    repo_identity TEXT NOT NULL,
    issue INTEGER NOT NULL,
    session TEXT NOT NULL DEFAULT '',
    checkout_path TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    branch TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    heartbeat_at INTEGER NOT NULL,
    release_reason TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS worktree_leases_active_repo_path_idx
    ON worktree_leases (repo_identity, worktree_path) WHERE status = 'active';

CREATE INDEX IF NOT EXISTS worktree_leases_repo_status_idx
    ON worktree_leases (repo_identity, status);
";

/// Create the table plus both additive migrations, in open order.
pub(super) fn ensure_lease_schema(storage: &Storage) -> Result<(), String> {
    storage
        .connection
        .execute_batch(WORKTREE_LEASES_SCHEMA)
        .map_err(|error| format!("could not initialise worktree lease table: {error}"))?;
    ensure_release_reason_column(storage)?;
    ensure_active_path_index(storage)
}

/// Migrate the `(repo_identity, worktree_path)` uniqueness to
/// active-only (issue 628 P4): at most one `active` lease per checkout,
/// while `retained` / `released` history never blocks a new active
/// lease on the same path. Idempotent: the old full-table index is
/// dropped when present and the partial index is created when absent,
/// so pre-P4 databases migrate on open with no data loss and fresh
/// databases converge on the same shape. Safe because the old index
/// could never hold two rows for one path, so no two `active` rows
/// for one path can pre-exist.
///
/// The drop and the create run inside one `BEGIN IMMEDIATE`
/// transaction. Without it two concurrent migrators interleave a
/// committed window with *no* path index at all (the old one dropped,
/// the new one not yet created), during which a racing insert is
/// completely unenforced. `IMMEDIATE` takes the write lock up front
/// (waiting on `busy_timeout` under contention) so concurrent
/// migrators serialize and the index gap is never committed. A cheap
/// `sqlite_master` fast path skips the write transaction entirely when
/// the database already carries the partial index without the legacy
/// one, keeping the hot no-op path lock-free.
fn ensure_active_path_index(storage: &Storage) -> Result<(), String> {
    if migration_complete(&storage.connection)
        .map_err(|error| format!("could not inspect worktree lease indexes: {error}"))?
    {
        return Ok(());
    }
    let migrated = if storage.connection.is_autocommit() {
        storage.connection.execute_batch(
            "BEGIN IMMEDIATE;
         DROP INDEX IF EXISTS worktree_leases_repo_path_idx;
         CREATE UNIQUE INDEX IF NOT EXISTS worktree_leases_active_repo_path_idx
             ON worktree_leases (repo_identity, worktree_path) WHERE status = 'active';
         COMMIT;",
        )
    } else {
        // Already inside the caller's transaction (no production caller
        // does this today): the outer transaction governs atomicity, so
        // run the idempotent statements bare rather than nesting a
        // transaction the migrator does not own.
        storage.connection.execute_batch(
            "DROP INDEX IF EXISTS worktree_leases_repo_path_idx;
             CREATE UNIQUE INDEX IF NOT EXISTS worktree_leases_active_repo_path_idx
                 ON worktree_leases (repo_identity, worktree_path) WHERE status = 'active';",
        )
    };
    if let Err(error) = migrated {
        // `execute_batch` stops at the first failing statement but
        // leaves the explicit transaction open; roll it back so the
        // connection never leaks an aborted write transaction into
        // the caller's later statements. A rollback error just means
        // there was nothing to roll back.
        let _ = storage.connection.execute_batch("ROLLBACK;");
        return Err(format!("could not migrate worktree lease index: {error}"));
    }
    Ok(())
}

/// True when the partial active-only index exists and the legacy
/// full-table index is gone, so the migration is a no-op.
fn migration_complete(connection: &rusqlite::Connection) -> Result<bool, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name IN \
         ('worktree_leases_repo_path_idx', 'worktree_leases_active_repo_path_idx')",
    )?;
    let names: Vec<String> = statement
        .query_map([], |row| row.get(0))?
        .filter_map(|row| row.ok())
        .collect();
    Ok(!names.contains(&"worktree_leases_repo_path_idx".to_owned())
        && names.contains(&"worktree_leases_active_repo_path_idx".to_owned()))
}

/// Additive migration for `worktree_leases.release_reason`: databases
/// created before the force-release surface gain a NULL column on
/// open. Idempotent via `PRAGMA table_info`, mirroring the storage
/// `MIGRATIONS` runner without pulling it in.
fn ensure_release_reason_column(storage: &Storage) -> Result<(), String> {
    let mut statement = storage
        .connection
        .prepare(
            "SELECT name FROM pragma_table_info('worktree_leases') WHERE name = 'release_reason'",
        )
        .map_err(|error| format!("could not inspect worktree lease table: {error}"))?;
    let present: bool = statement
        .exists([])
        .map_err(|error| format!("could not inspect worktree lease table: {error}"))?;
    if !present {
        storage
            .connection
            .execute(
                "ALTER TABLE worktree_leases ADD COLUMN release_reason TEXT",
                [],
            )
            .map_err(|error| format!("could not migrate worktree lease table: {error}"))?;
    }
    Ok(())
}
