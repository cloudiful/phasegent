use super::support::*;
use super::*;

#[test]
fn release_flips_lease_to_retained_or_released_and_is_idempotent() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("release") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("release");
    let cache = unique_cache("release");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("acquire");
    let retain = release_lease(&outcome.lease_id, true).expect("release retain");
    assert_eq!(retain.status, LEASE_STATUS_RETAINED);
    let second_retain = release_lease(&outcome.lease_id, true).expect("release retain again");
    assert_eq!(second_retain.status, LEASE_STATUS_RETAINED);
    let missing = release_lease("lease-does-not-exist", true);
    let error = missing.expect_err("missing lease must error");
    assert_eq!(error.kind, "state");
    drop(cache);
    drop(db_temp);
}

#[test]
fn release_can_flip_a_lease_to_released() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("release-non-retain") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("release-non-retain");
    let cache = unique_cache("release-non-retain");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("acquire");
    let released = release_lease(&outcome.lease_id, false).expect("release released");
    assert_eq!(released.status, LEASE_STATUS_RELEASED);
    assert!(!released.forced);
    assert_eq!(released.reason, None);
    drop(cache);
    drop(db_temp);
}

#[test]
fn forced_release_records_reason_and_keeps_row() {
    // `release --force --reason` persists the justification on the
    // row so the override stays attributable; the row itself is
    // never deleted.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("release-forced") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("release-forced");
    let cache = unique_cache("release-forced");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("acquire");
    let forced = release_lease_forced(&outcome.lease_id, true, "stuck session cleanup")
        .expect("forced release");
    assert_eq!(forced.status, LEASE_STATUS_RETAINED);
    assert!(forced.forced);
    assert_eq!(forced.reason.as_deref(), Some("stuck session cleanup"));

    let identity = repo_identity(&runner, repo.dir.path()).expect("repo identity");
    let listed = leases_for_repo(&identity).expect("list leases");
    let row = listed
        .iter()
        .find(|row| row.lease_id == outcome.lease_id)
        .expect("released row must remain visible");
    assert_eq!(row.release_reason.as_deref(), Some("stuck session cleanup"));

    // Forcing an already-terminal lease is a no-op that records nothing.
    let again =
        release_lease_forced(&outcome.lease_id, true, "second try").expect("second forced release");
    assert!(!again.forced);
    assert_eq!(again.reason, None);
    drop(cache);
    drop(db_temp);
}
