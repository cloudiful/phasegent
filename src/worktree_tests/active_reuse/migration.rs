//! Legacy-index migration coverage (issue 628 P4).
//!
//! Moved verbatim from the parent `active_reuse` module; the shared
//! `active_lease` helper stays in the parent. Every test runs against
//! isolated temp databases under the workflow lock; the live store is
//! never touched.

use super::*;

#[test]
fn legacy_full_table_index_migrates_without_data_loss() {
    // Simulate a pre-P4 database: old full-table unique index plus a
    // retained row that used to block the checkout forever.
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("active-migrate");
    storage
        .connection
        .execute_batch(
            "CREATE TABLE worktree_leases (
                lease_id TEXT PRIMARY KEY, repo_identity TEXT NOT NULL,
                issue INTEGER NOT NULL, session TEXT NOT NULL DEFAULT '',
                checkout_path TEXT NOT NULL, worktree_path TEXT NOT NULL,
                branch TEXT NOT NULL, status TEXT NOT NULL,
                created_at INTEGER NOT NULL, heartbeat_at INTEGER NOT NULL,
                release_reason TEXT
            );
            CREATE UNIQUE INDEX worktree_leases_repo_path_idx
                ON worktree_leases (repo_identity, worktree_path);",
        )
        .expect("legacy schema");
    let now = now_unix_secs();
    storage
        .connection
        .execute(
            "INSERT INTO worktree_leases VALUES \
             ('lease-legacy', '/tmp/repo', 1, 's', '/tmp/repo', '/tmp/repo', \
              'phasegent/1-aaaaaa', 'retained', ?1, ?1, NULL)",
            rusqlite::params![now],
        )
        .expect("legacy terminal row");
    drop(storage);
    // Reopen through the real path so the migration under test runs.
    let storage =
        Storage::open_at(&temp.path().join("phasegent.sqlite3")).expect("reopen must migrate");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema migrates");
    let old: bool = storage
        .connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'index' AND name = 'worktree_leases_repo_path_idx'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("index census")
        == 1;
    assert!(!old, "the legacy full-table index must be gone");
    let partial: bool = storage
        .connection
        .query_row(
            "SELECT sql FROM sqlite_master \
             WHERE type = 'index' AND name = 'worktree_leases_active_repo_path_idx'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("partial index must exist")
        .contains("WHERE status = 'active'");
    assert!(partial, "the replacement index must be active-only");
    insert_lease(
        &storage,
        active_lease("lease-after", "/tmp/repo", "/tmp/repo", 2, now),
    )
    .expect("post-migration reuse of a retained checkout");
    let rows = leases_for_repo("/tmp/repo").expect("repo list");
    assert_eq!(rows.len(), 2, "legacy history survives migration");
    drop(temp);
}

const LEGACY_SCHEMA: &str = "CREATE TABLE worktree_leases (
    lease_id TEXT PRIMARY KEY, repo_identity TEXT NOT NULL,
    issue INTEGER NOT NULL, session TEXT NOT NULL DEFAULT '',
    checkout_path TEXT NOT NULL, worktree_path TEXT NOT NULL,
    branch TEXT NOT NULL, status TEXT NOT NULL,
    created_at INTEGER NOT NULL, heartbeat_at INTEGER NOT NULL,
    release_reason TEXT
);
CREATE UNIQUE INDEX worktree_leases_repo_path_idx
    ON worktree_leases (repo_identity, worktree_path);";

/// Count the legacy and partial path indexes. Returns `None` on any
/// read failure (a busy snapshot during commit) so the census spinner
/// skips the sample instead of reporting a phantom gap.
fn index_census(connection: &rusqlite::Connection) -> Option<(i64, i64)> {
    connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'worktree_leases_repo_path_idx'),
                (SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'worktree_leases_active_repo_path_idx')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
}

#[test]
fn concurrent_migration_never_exposes_index_gap() {
    // P4 hardening: the old→partial cutover runs in one IMMEDIATE
    // transaction, so a census reader must never observe the legacy
    // index gone while the partial one is still absent. Each iteration
    // rebuilds the legacy schema, then races the real
    // `ensure_schema` migration against a tight census spinner.
    use std::sync::atomic::{AtomicBool, Ordering};
    let _lock = lock_workflow_tests();
    let temp = TempDir::new("migration-gap");
    let db = temp.path().join("phasegent.sqlite3");
    for iteration in 0..60 {
        {
            let storage = Storage::open_at(&db).expect("control open");
            storage
                .connection
                .execute_batch("DROP TABLE IF EXISTS worktree_leases;")
                .expect("reset table");
            storage
                .connection
                .execute_batch(LEGACY_SCHEMA)
                .expect("legacy schema");
            storage
                .connection
                .execute(
                    "INSERT INTO worktree_leases VALUES \
                     ('lease-legacy', '/tmp/repo', 1, 's', '/tmp/repo', '/tmp/repo', \
                      'phasegent/1-aaaaaa', 'retained', ?1, ?1, NULL)",
                    rusqlite::params![now_unix_secs()],
                )
                .expect("legacy terminal row");
        }
        let gap = AtomicBool::new(false);
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let storage = Storage::open_at(&db).expect("census open");
                while !stop.load(Ordering::Relaxed) {
                    if index_census(&storage.connection) == Some((0, 0)) {
                        gap.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            });
            let storage = Storage::open_at(&db).expect("migrator open");
            crate::worktree::ensure_schema(&storage).expect("migration must succeed");
            stop.store(true, Ordering::Relaxed);
        });
        assert!(
            !gap.load(Ordering::Relaxed),
            "iteration {iteration}: census saw neither index mid-migration"
        );
        let storage = Storage::open_at(&db).expect("verify open");
        assert_eq!(
            index_census(&storage.connection),
            Some((0, 1)),
            "iteration {iteration}: exactly the partial index must remain"
        );
        let retained: i64 = storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM worktree_leases WHERE lease_id = 'lease-legacy'",
                [],
                |row| row.get(0),
            )
            .expect("history row must survive");
        assert_eq!(retained, 1, "iteration {iteration}: history preserved");
    }
    drop(temp);
}
