use super::support::*;
use super::*;

#[test]
fn legacy_stale_recovery_rejects_a_newly_corrupt_scoped_row() {
    use crate::worktree::leases::{
        NewLease, StaleRecoveryScope, insert_lease_scoped, list_all_for_repo,
        recover_stale_active_leases_for_scope,
    };

    let _lock = lock_workflow_tests();
    let (_temp, mut storage, _env) = open_temp_db("stale-corrupt-review");
    crate::worktree::ensure_schema(&storage).unwrap();
    let scope = crate::worktree::LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    insert_lease_scoped(
        &storage,
        NewLease {
            lease_id: "scoped",
            identity: "repo",
            issue: 7,
            session: "s",
            checkout_path: "checkout",
            worktree_path: "path",
            branch: "branch",
            status: LEASE_STATUS_ACTIVE,
            created_at: 1,
            heartbeat_at: 1,
        },
        &scope,
    )
    .unwrap();
    crate::worktree::lease_scope::scope_map_for_repo(&storage, "repo").unwrap();
    storage
        .connection
        .execute(
            "UPDATE worktree_lease_scopes SET provider = 'FORGEJO' WHERE lease_id = 'scoped'",
            [],
        )
        .unwrap();
    let result = recover_stale_active_leases_for_scope(
        &mut storage,
        "repo",
        50,
        "fixture",
        100,
        StaleRecoveryScope::LegacyOnly,
    );
    assert_eq!(
        list_all_for_repo(&storage, "repo").unwrap()[0].status,
        LEASE_STATUS_ACTIVE,
        "invalid sidecar cannot become an unscoped legacy lease"
    );
    assert!(result.is_err() || result.as_ref().is_ok_and(Vec::is_empty));
}

#[test]
fn scope_schema_is_additive_and_inserts_atomically() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("scope-schema");
    crate::worktree::ensure_schema(&storage).unwrap();
    let now = crate::worktree::now_unix_secs();
    let lease = crate::worktree::leases::NewLease {
        lease_id: "scope-a",
        identity: "repo",
        issue: 7,
        session: "s",
        checkout_path: "checkout",
        worktree_path: "path",
        branch: "branch",
        status: LEASE_STATUS_ACTIVE,
        created_at: now,
        heartbeat_at: now,
    };
    let scope = crate::worktree::LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
    crate::worktree::leases::insert_lease_scoped(&storage, lease, &scope).unwrap();
    crate::worktree::ensure_schema(&storage).unwrap();
    assert_eq!(
        crate::worktree::lease_scope::scope_map_for_repo(&storage, "repo")
            .unwrap()
            .get("scope-a"),
        Some(&scope)
    );
    assert!(
        crate::worktree::leases::find_active_lease_for_scope(&storage, "repo", "s", &scope)
            .unwrap()
            .is_some()
    );
    assert!(
        crate::worktree::leases::find_active_lease_for_probe(&storage, "repo", 7, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        crate::worktree::leases::retain_active_leases_for_issue(&storage, "repo", 7, "legacy", now)
            .unwrap(),
        0
    );
    assert_eq!(
        crate::worktree::leases::retain_active_leases_for_scope(
            &storage, "repo", &scope, "scope", now
        )
        .unwrap(),
        1
    );
    assert_eq!(
        crate::worktree::leases::list_all_for_repo(&storage, "repo")
            .unwrap()
            .len(),
        1
    );
    assert!(crate::worktree::LeaseScope::new("forgejo", "owner/repo", 0).is_err());
    assert!(crate::worktree::LeaseScope::new("forgejo", "owner/ repo", 7).is_err());
    storage.connection.execute_batch("CREATE TRIGGER reject_scope BEFORE INSERT ON worktree_lease_scopes BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;").unwrap();
    let lease = crate::worktree::leases::NewLease {
        lease_id: "scope-b",
        identity: "repo",
        issue: 7,
        session: "s2",
        checkout_path: "checkout",
        worktree_path: "path2",
        branch: "branch",
        status: LEASE_STATUS_ACTIVE,
        created_at: now,
        heartbeat_at: now,
    };
    assert!(crate::worktree::leases::insert_lease_scoped(&storage, lease, &scope).is_err());
    assert!(
        crate::worktree::leases::load_lease(&storage, "scope-b")
            .unwrap()
            .is_none()
    );
    drop(temp);
}

#[test]
fn scope_exact_identity_and_legacy_collision_are_isolated() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("scope-isolation");
    crate::worktree::ensure_schema(&storage).unwrap();
    let now = now_unix_secs();
    let make = |id, path, issue| crate::worktree::leases::NewLease {
        lease_id: id,
        identity: "repo",
        issue,
        session: "session",
        checkout_path: "checkout",
        worktree_path: path,
        branch: "branch",
        status: LEASE_STATUS_ACTIVE,
        created_at: now,
        heartbeat_at: now,
    };
    let a = crate::worktree::LeaseScope::new("forgejo", "one/repo", 7).unwrap();
    let b = crate::worktree::LeaseScope::new("forgejo", "two/repo", 7).unwrap();
    crate::worktree::leases::insert_lease_scoped(&storage, make("a", "path-a", 7), &a).unwrap();
    crate::worktree::leases::insert_lease_scoped(&storage, make("b", "path-b", 7), &b).unwrap();
    assert_eq!(
        crate::worktree::leases::retain_active_leases_for_scope(&storage, "repo", &a, "done", now)
            .unwrap(),
        1
    );
    assert!(
        crate::worktree::leases::find_active_lease_for_scope(&storage, "repo", "session", &a)
            .unwrap()
            .is_none()
    );
    assert!(
        crate::worktree::leases::find_active_lease_for_scope(&storage, "repo", "session", &b)
            .unwrap()
            .is_some()
    );
    assert!(
        crate::worktree::leases::insert_lease_scoped(&storage, make("c", "path-b", 7), &a).is_err()
    );
    crate::worktree::leases::insert_lease(&storage, make("legacy", "legacy-path", 9)).unwrap();
    let legacy_scope = crate::worktree::LeaseScope::new("forgejo", "one/repo", 9).unwrap();
    let error = crate::worktree::leases::insert_lease_scoped(
        &storage,
        make("conflict", "new-path", 9),
        &legacy_scope,
    )
    .unwrap_err();
    assert_eq!(error.kind, "scope_conflict");
    assert!(
        crate::worktree::leases::load_lease(&storage, "conflict")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        crate::worktree::leases::list_all_for_repo(&storage, "repo")
            .unwrap()
            .len(),
        3
    );
    drop(temp);
}

#[test]
fn scope_concurrent_active_path_has_one_complete_winner() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("scope-race");
    crate::worktree::ensure_schema(&storage).unwrap();
    let path = temp.path().join("phasegent.sqlite3");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|index| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let db = crate::infra::storage::Storage::open_at(&path).unwrap();
                let id = format!("race-{index}");
                let scope = crate::worktree::LeaseScope::new("forgejo", "owner/repo", 7).unwrap();
                let lease = crate::worktree::leases::NewLease {
                    lease_id: &id,
                    identity: "repo",
                    issue: 7,
                    session: "s",
                    checkout_path: "checkout",
                    worktree_path: "same-path",
                    branch: "branch",
                    status: LEASE_STATUS_ACTIVE,
                    created_at: 1,
                    heartbeat_at: 1,
                };
                barrier.wait();
                crate::worktree::leases::insert_lease_scoped(&db, lease, &scope).is_ok()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
    assert_eq!(
        crate::worktree::leases::list_all_for_repo(&storage, "repo")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        crate::worktree::lease_scope::scope_map_for_repo(&storage, "repo")
            .unwrap()
            .len(),
        1
    );
    drop(storage);
    drop(temp);
}

#[test]
fn ensure_schema_is_idempotent_and_creates_expected_columns() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("schema");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema first call");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema second call");
    let mut statement = storage
        .connection
        .prepare("PRAGMA table_info(worktree_leases)")
        .expect("pragma");
    let columns: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query")
        .filter_map(|row| row.ok())
        .collect();
    for expected in [
        "lease_id",
        "repo_identity",
        "issue",
        "session",
        "checkout_path",
        "worktree_path",
        "branch",
        "status",
        "created_at",
        "heartbeat_at",
    ] {
        assert!(
            columns.contains(&expected.to_owned()),
            "missing column {expected}"
        );
    }
    drop(temp);
}

#[test]
fn leases_for_issue_and_repo_return_only_matching_active_rows() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("list");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    let now = crate::worktree::now_unix_secs();
    let identity = "/tmp/repo-1";
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-a",
            identity,
            issue: 239,
            session: "session-1",
            checkout_path: "/tmp/repo-1",
            worktree_path: "/tmp/repo-1/wt-a",
            branch: "phasegent/239-aaaaaa",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("insert a");
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-b",
            identity,
            issue: 239,
            session: "session-2",
            checkout_path: "/tmp/repo-1",
            worktree_path: "/tmp/repo-1/wt-b",
            branch: "phasegent/239-bbbbbb",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("insert b");
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-c",
            identity: "/tmp/repo-2",
            issue: 239,
            session: "session-1",
            checkout_path: "/tmp/repo-2",
            worktree_path: "/tmp/repo-2",
            branch: "phasegent/239-cccccc",
            status: LEASE_STATUS_RETAINED,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("insert c");
    let issue_rows = leases_for_issue(239).expect("issue list");
    assert_eq!(issue_rows.len(), 2);
    assert!(
        issue_rows
            .iter()
            .all(|row| row.status == LEASE_STATUS_ACTIVE)
    );
    assert!(issue_rows.iter().all(|row| row.issue == 239));
    let repo_rows = leases_for_repo(identity).expect("repo list");
    assert_eq!(repo_rows.len(), 2);
    assert!(repo_rows.iter().all(|row| row.repo_identity == identity));
    drop(temp);
}

#[test]
fn unique_index_blocks_duplicate_repo_path_rows() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("unique");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    let now = crate::worktree::now_unix_secs();
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-dup",
            identity: "/tmp/repo",
            issue: 1,
            session: "s",
            checkout_path: "/tmp/repo",
            worktree_path: "/tmp/repo",
            branch: "phasegent/1-aaaaaa",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("first insert");
    let result = crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "lease-dup-2",
            identity: "/tmp/repo",
            issue: 2,
            session: "s",
            checkout_path: "/tmp/repo",
            worktree_path: "/tmp/repo",
            branch: "phasegent/2-bbbbbb",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    );
    let error = result.expect_err("duplicate (repo_identity, worktree_path) must error");
    assert_eq!(
        error.kind, "storage",
        "the rewrite must not change the error kind (the CLI exit code keys on it)"
    );
    assert!(
        error.message.contains("--isolate")
            && error.message.contains("worktree status")
            && error.message.contains("worktree list"),
        "a repo/path collision must name --isolate and the status/list surfaces: {error}"
    );
    assert!(
        !error.message.contains("UNIQUE constraint failed")
            && !error.message.contains("worktree_leases_repo_path_idx"),
        "raw SQLite index text must never reach the operator: {error}"
    );
    drop(temp);
}
