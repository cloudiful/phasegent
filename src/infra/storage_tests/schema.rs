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

/// The retired ACP research ledger is gone from a fresh database: opening a
/// database must create no `acp_research_*` (or older `acp_explorer_*`) table
/// or index, so no schema entry or migration re-registers the removed backend.
#[test]
fn fresh_database_creates_no_research_ledger() {
    let (temp_dir, storage) = open_at_temp("research-removed");
    for table in [
        "acp_research_runs",
        "acp_research_run_owners",
        "acp_explorer_runs",
        "acp_explorer_run_owners",
    ] {
        assert!(
            !table_exists(&storage, table),
            "fresh database must not create the removed table {table}"
        );
    }
    for index in [
        "acp_research_runs_status_idx",
        "acp_research_run_owners_owner_idx",
        "acp_explorer_runs_status_idx",
        "acp_explorer_run_owners_owner_idx",
    ] {
        assert!(
            !index_exists(&storage, index),
            "fresh database must not create the removed index {index}"
        );
    }
    let _ = fs::remove_dir_all(temp_dir);
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
