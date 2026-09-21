use super::cli_support::*;
use super::support::*;
use super::*;

#[test]
fn parse_heartbeat_requires_lease_and_parses_session() {
    let invocation = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "heartbeat",
        "--lease",
        "lease-1",
        "--session",
        "alpha",
    ]))
    .unwrap();
    match invocation.command {
        Command::Worktree(WorktreeCommand::Heartbeat { lease, session }) => {
            assert_eq!(lease, "lease-1");
            assert_eq!(session.as_deref(), Some("alpha"));
        }
        other => panic!("unexpected command {other:?}"),
    }
    let missing = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "heartbeat",
    ]))
    .unwrap_err();
    assert!(missing.contains("--lease"), "unexpected error: {missing}");
}

#[test]
fn parse_heartbeat_rejects_blank_session() {
    let error = crate::command::parse(&strings([
        "--role",
        "orchestrator",
        "worktree",
        "heartbeat",
        "--lease",
        "lease-1",
        "--session",
        "",
    ]))
    .unwrap_err();
    assert!(error.contains("session"), "unexpected error: {error}");
}

#[test]
fn cli_heartbeat_refreshes_owned_active_lease() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("cli-heartbeat");
    ensure_schema(&storage).expect("schema");
    let old = now_unix_secs() - 1000;
    insert_lease_for_identity(
        &storage,
        "lease-hb",
        "/tmp/repo",
        "session-A",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/repo",
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Heartbeat {
            lease: "lease-hb".to_owned(),
            session: Some("session-A".to_owned()),
        },
    );
    assert_eq!(exit, 0, "owned heartbeat must succeed");
    let row = list_for_repo(&storage, "/tmp/repo")
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-hb")
        .expect("row");
    assert!(row.heartbeat_at > old, "heartbeat must advance");
}

#[test]
fn cli_heartbeat_rejects_foreign_session_without_mutation() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("cli-heartbeat-foreign");
    ensure_schema(&storage).expect("schema");
    let old = now_unix_secs() - 1000;
    insert_lease_for_identity(
        &storage,
        "lease-hb",
        "/tmp/repo",
        "session-A",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/repo",
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Heartbeat {
            lease: "lease-hb".to_owned(),
            session: Some("session-B".to_owned()),
        },
    );
    assert_eq!(exit, 1, "foreign session is a structured state conflict");
    let row = list_for_repo(&storage, "/tmp/repo")
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-hb")
        .expect("row");
    assert_eq!(row.heartbeat_at, old, "conflict must not mutate the row");
}

#[test]
fn cli_heartbeat_uses_environment_session() {
    let _lock = lock_workflow_tests();
    let (_temp, storage, _env) = open_temp_db("cli-heartbeat-env");
    ensure_schema(&storage).expect("schema");
    let _session_env = EnvGuard::set("PHASEGENT_SESSION_ID", "env-session");
    let old = now_unix_secs() - 1000;
    insert_lease_for_identity(
        &storage,
        "lease-hb",
        "/tmp/repo",
        "env-session",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/repo",
    );
    let exit = execute_worktree(
        Some(Role::Orchestrator),
        WorktreeCommand::Heartbeat {
            lease: "lease-hb".to_owned(),
            session: None,
        },
    );
    assert_eq!(exit, 0, "environment session must own the lease");
    let row = list_for_repo(&storage, "/tmp/repo")
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-hb")
        .expect("row");
    assert!(row.heartbeat_at > old, "heartbeat must advance");
}
