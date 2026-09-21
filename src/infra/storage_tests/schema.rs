use super::support::*;
use super::*;

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
