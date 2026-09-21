use super::support::*;
use super::*;

#[test]
fn heartbeat_refreshes_owned_active_lease() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("heartbeat-own") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("heartbeat-own");
    let cache = unique_cache("heartbeat-own");
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
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let before = heartbeat_of(&identity, &outcome.lease_id);
    let refreshed =
        heartbeat_lease(&outcome.lease_id, "session-A", before + 100).expect("owned heartbeat");
    assert_eq!(refreshed.status, LEASE_STATUS_ACTIVE);
    assert_eq!(refreshed.heartbeat_at, before + 100);
    assert_eq!(heartbeat_of(&identity, &outcome.lease_id), before + 100);
    drop(cache);
    drop(db_temp);
}

#[test]
fn heartbeat_rejects_foreign_session_without_mutation() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("heartbeat-foreign") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("heartbeat-foreign");
    let cache = unique_cache("heartbeat-foreign");
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
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let before = heartbeat_of(&identity, &outcome.lease_id);
    let error = heartbeat_lease(&outcome.lease_id, "session-B", before + 500)
        .expect_err("foreign session must conflict");
    assert_eq!(error.kind, "state");
    assert!(error.message.contains("session"), "unexpected: {error}");
    assert_eq!(
        heartbeat_of(&identity, &outcome.lease_id),
        before,
        "a foreign heartbeat must not move the row"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn heartbeat_rejects_terminal_and_missing_leases() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("heartbeat-terminal") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("heartbeat-terminal");
    let cache = unique_cache("heartbeat-terminal");
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
    release_lease(&outcome.lease_id, true).expect("retain");
    let terminal = heartbeat_lease(&outcome.lease_id, "session-A", now_unix_secs() + 10)
        .expect_err("terminal lease must conflict");
    assert_eq!(terminal.kind, "state");
    assert!(
        terminal.message.contains("not active"),
        "unexpected: {terminal}"
    );
    let missing = heartbeat_lease("lease-does-not-exist", "session-A", now_unix_secs())
        .expect_err("unknown lease must conflict");
    assert_eq!(missing.kind, "state");
    assert!(
        missing.message.contains("not found"),
        "unexpected: {missing}"
    );
    drop(cache);
    drop(db_temp);
}
