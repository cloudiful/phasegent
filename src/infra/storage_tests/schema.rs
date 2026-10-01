use super::support::*;
use super::*;
use rusqlite::OptionalExtension;

#[test]
fn open_creates_database_with_private_permissions() {
    let (temp_dir, storage) = open_at_temp("open");
    let db_path = storage.db_path();
    assert!(db_path.exists(), "database file must exist after open");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory_mode = fs::metadata(db_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700, "config directory must be 0700");
        let file_mode = fs::metadata(db_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "database file must be 0600");
    }
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn schema_initialisation_is_idempotent() {
    let (temp_dir, storage) = open_at_temp("schema");
    drop(storage);
    // Second open must reuse the existing database without error and
    // continue to expose a usable Storage handle.
    let storage = Storage::open_at(&temp_dir.join(DB_FILENAME)).unwrap();
    assert!(storage.load_role_config(Role::Admin).unwrap().is_none());
    let _ = fs::remove_dir_all(temp_dir);
}

/// A fresh database creates only the canonical issue-692 research ledger
/// names: no legacy `acp_explorer_*` table, column, or index may be created.
#[test]
fn fresh_database_creates_only_the_canonical_research_ledger() {
    let (temp_dir, storage) = open_at_temp("research-fresh");
    for table in [
        "acp_research_runs",
        "acp_research_run_owners",
        "acp_explorer_runs",
        "acp_explorer_run_owners",
    ] {
        assert_eq!(
            table_exists(&storage, table),
            table.starts_with("acp_research"),
            "fresh database table {table}"
        );
    }
    assert_eq!(
        column_names(&storage, "acp_research_runs"),
        vec![
            "run_id",
            "status",
            "scratch_cwd",
            "acp_session_id",
            "prompt",
            "output",
            "output_truncated",
            "error",
            "created_at",
            "updated_at",
            "finished_at",
        ],
        "the fresh research run table must carry scratch_cwd and no legacy column"
    );
    for index in [
        "acp_research_runs_status_idx",
        "acp_research_run_owners_owner_idx",
        "acp_explorer_runs_status_idx",
        "acp_explorer_run_owners_owner_idx",
    ] {
        assert_eq!(
            index_exists(&storage, index),
            index.starts_with("acp_research"),
            "fresh database index {index}"
        );
    }
    let _ = fs::remove_dir_all(temp_dir);
}

/// A pre-issue-692 database keeps every pending/running run row, owner row,
/// and value across the rename, and the migration is idempotent on reopen.
#[test]
fn legacy_research_ledger_migrates_in_place_and_is_idempotent() {
    let temp_dir = unique_temp_dir("research-legacy");
    let db_path = temp_dir.join(DB_FILENAME);
    seed_legacy_research_db(&db_path);

    let storage = Storage::open_at(&db_path).unwrap();
    assert_research_rows_preserved(&storage);
    drop(storage);

    // A second open must be a no-op: the canonical tables exist, the legacy
    // ones do not, and nothing is discarded.
    let storage = Storage::open_at(&db_path).unwrap();
    assert_research_rows_preserved(&storage);
    for table in ["acp_explorer_runs", "acp_explorer_run_owners"] {
        assert!(
            !table_exists(&storage, table),
            "legacy table {table} must be gone after migration"
        );
    }
    for index in [
        "acp_explorer_runs_status_idx",
        "acp_explorer_run_owners_owner_idx",
    ] {
        assert!(
            !index_exists(&storage, index),
            "legacy index {index} must be dropped after migration"
        );
    }
    for index in [
        "acp_research_runs_status_idx",
        "acp_research_run_owners_owner_idx",
    ] {
        assert!(
            index_exists(&storage, index),
            "canonical index {index} must exist after migration"
        );
    }
    let _ = fs::remove_dir_all(temp_dir);
}

/// The research-ledger rename serializes with the same immediate-transaction
/// busy retry as the additive migrations, so two concurrent opens of a legacy
/// database both succeed and both observe the preserved row.
#[test]
fn research_ledger_migration_tolerates_concurrent_opens() {
    let temp_dir = unique_temp_dir("research-legacy-concurrent");
    let db_path = temp_dir.join(DB_FILENAME);
    seed_legacy_research_db(&db_path);

    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = db_path.clone();
            std::thread::spawn(move || Storage::open_at(&path))
        })
        .collect();
    for handle in handles {
        let storage = handle.join().unwrap().unwrap();
        assert_research_rows_preserved(&storage);
    }
    let _ = fs::remove_dir_all(temp_dir);
}

fn seed_legacy_research_db(path: &std::path::Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;
         CREATE TABLE IF NOT EXISTS acp_explorer_runs (
             run_id TEXT PRIMARY KEY,
             status TEXT NOT NULL,
             worktree_cwd TEXT NOT NULL,
             acp_session_id TEXT,
             prompt TEXT NOT NULL,
             output TEXT,
             output_truncated INTEGER NOT NULL DEFAULT 0,
             error TEXT,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             finished_at INTEGER
         );
         CREATE TABLE IF NOT EXISTS acp_explorer_run_owners (
             run_id TEXT PRIMARY KEY,
             owner_session_id TEXT NOT NULL,
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS acp_explorer_run_owners_owner_idx
             ON acp_explorer_run_owners (owner_session_id);
         CREATE INDEX IF NOT EXISTS acp_explorer_runs_status_idx
             ON acp_explorer_runs (status, created_at DESC);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO acp_explorer_runs \
             (run_id, status, worktree_cwd, acp_session_id, prompt, output, output_truncated, \
              error, created_at, updated_at, finished_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            "legacy-run",
            "running",
            "/legacy/worktree",
            "acp-session-1",
            "summarize the topic",
            "partial output",
            1_i64,
            "previous error",
            1_700_000_000_i64,
            1_700_000_100_i64,
            Option::<i64>::None,
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO acp_explorer_run_owners (run_id, owner_session_id, created_at) \
         VALUES (?1, ?2, ?3)",
        rusqlite::params!["legacy-run", "ses_legacy_owner", 1_700_000_000_i64],
    )
    .unwrap();
}

fn assert_research_rows_preserved(storage: &Storage) {
    assert!(
        table_exists(storage, "acp_research_runs"),
        "the canonical run table must exist after migration"
    );
    let run = storage.load_work_run("legacy-run").unwrap().unwrap();
    assert_eq!(run.status, "running");
    assert_eq!(run.scratch_cwd, "/legacy/worktree");
    assert_eq!(run.acp_session_id.as_deref(), Some("acp-session-1"));
    assert_eq!(run.prompt, "summarize the topic");
    assert_eq!(run.output.as_deref(), Some("partial output"));
    assert!(run.output_truncated);
    assert_eq!(run.error.as_deref(), Some("previous error"));
    assert_eq!(run.created_at, 1_700_000_000);
    assert_eq!(run.updated_at, 1_700_000_100);
    assert!(run.finished_at.is_none());
    assert!(
        crate::mcp::agent::store::owner::run_is_owned_by(storage, "legacy-run", "ses_legacy_owner")
            .unwrap(),
        "the legacy owner row must survive the rename"
    );
    assert!(
        !crate::mcp::agent::store::owner::run_is_owned_by(storage, "legacy-run", "ses_other")
            .unwrap(),
        "ownership must stay exact after the rename"
    );
}

fn table_exists(storage: &Storage, table: &str) -> bool {
    storage
        .connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            rusqlite::params![table],
            |_| Ok(()),
        )
        .optional()
        .unwrap()
        .is_some()
}

fn index_exists(storage: &Storage, index: &str) -> bool {
    storage
        .connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1",
            rusqlite::params![index],
            |_| Ok(()),
        )
        .optional()
        .unwrap()
        .is_some()
}

fn column_names(storage: &Storage, table: &str) -> Vec<String> {
    let mut statement = storage
        .connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(|name| name.unwrap())
        .collect()
}
