use super::support::*;
use super::*;

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
