//! Active-only `(repo_identity, worktree_path)` uniqueness (issue 628
//! P4): terminal lease history never blocks a later active lease on
//! the same checkout, while two live leases for one checkout still
//! conflict. Old full-table indexes migrate on open with no data loss.

use super::support::*;
use super::*;

mod migration;

fn active_lease<'a>(
    id: &'a str,
    identity: &'a str,
    path: &'a str,
    issue: u64,
    now: i64,
) -> NewLease<'a> {
    NewLease {
        lease_id: id,
        identity,
        issue,
        session: "session-A",
        checkout_path: path,
        worktree_path: path,
        branch: "phasegent/1-aaaaaa",
        status: LEASE_STATUS_ACTIVE,
        created_at: now,
        heartbeat_at: now,
    }
}

#[test]
fn terminal_history_does_not_block_path_reuse() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("active-reuse");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    let now = now_unix_secs();
    insert_lease(
        &storage,
        active_lease("lease-old", "/tmp/repo", "/tmp/repo", 1, now),
    )
    .expect("first active insert");
    crate::worktree::leases::update_status(&storage, "lease-old", LEASE_STATUS_RETAINED)
        .expect("release to retained");
    insert_lease(
        &storage,
        active_lease("lease-new", "/tmp/repo", "/tmp/repo", 2, now),
    )
    .expect("retained history must not block a new active lease");
    let rows = leases_for_repo("/tmp/repo").expect("repo list");
    assert_eq!(rows.len(), 2, "both the history row and the live row stay");
    assert!(
        rows.iter().any(|row| row.status == LEASE_STATUS_RETAINED),
        "terminal history is preserved: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.status == LEASE_STATUS_ACTIVE && row.issue == 2),
        "the new active lease owns the checkout: {rows:?}"
    );
    drop(temp);
}

#[test]
fn simultaneous_active_paths_still_conflict() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("active-conflict");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    let now = now_unix_secs();
    insert_lease(
        &storage,
        active_lease("lease-one", "/tmp/repo", "/tmp/repo", 1, now),
    )
    .expect("first active insert");
    let error = insert_lease(
        &storage,
        active_lease("lease-two", "/tmp/repo", "/tmp/repo", 2, now),
    )
    .expect_err("two live leases for one checkout must conflict");
    assert_eq!(error.kind, "storage");
    assert!(
        error.message.contains("--isolate")
            && error.message.contains("worktree status")
            && error.message.contains("worktree list"),
        "guidance must survive the migration: {error}"
    );
    assert!(
        !error.message.contains("UNIQUE constraint failed")
            && !error.message.contains("worktree_leases_repo_path_idx")
            && !error
                .message
                .contains("worktree_leases_active_repo_path_idx"),
        "raw SQLite index text must never reach the operator: {error}"
    );
    drop(temp);
}

#[test]
fn retained_rows_may_share_a_path_with_one_active_row() {
    // Partial-index semantics directly: several terminal rows plus a
    // single active row coexist on one checkout.
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("active-partial");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    let now = now_unix_secs();
    for (id, status) in [
        ("lease-r1", LEASE_STATUS_RETAINED),
        ("lease-r2", LEASE_STATUS_RELEASED),
    ] {
        let mut lease = active_lease(id, "/tmp/repo", "/tmp/repo", 1, now);
        lease.status = status;
        insert_lease(&storage, lease).expect("terminal insert");
    }
    insert_lease(
        &storage,
        active_lease("lease-live", "/tmp/repo", "/tmp/repo", 9, now),
    )
    .expect("one active row alongside terminal history");
    drop(temp);
}

#[test]
fn racing_second_active_insert_loses_with_guidance() {
    // Two threads racing an active insert for one checkout serialize
    // on the partial unique index: exactly one wins, the loser gets
    // the actionable guidance (never a raw SQLite error, never two
    // live rows). WAL + busy_timeout keep the race deterministic.
    let _lock = lock_workflow_tests();
    let temp = TempDir::new("active-race");
    let db = temp.path().join("phasegent.sqlite3");
    Storage::open_at(&db).expect("seed open");
    let path = db.to_string_lossy().into_owned();
    let first = std::thread::scope(|scope| {
        let handles: Vec<_> = ["lease-race-a", "lease-race-b"]
            .into_iter()
            .map(|id| {
                scope.spawn({
                    let path = path.clone();
                    move || {
                        // SAFETY: the env override is installed before
                        // the scope spawns and removed after it joins,
                        // while the workflow lock is held throughout.
                        unsafe {
                            std::env::set_var("PHASEGENT_DB_PATH", &path);
                        }
                        let storage = Storage::open().expect("racer open");
                        crate::worktree::ensure_schema(&storage).expect("racer schema");
                        insert_lease(
                            &storage,
                            active_lease(id, "/tmp/repo", "/tmp/repo", 1, now_unix_secs()),
                        )
                        .map(|()| id)
                        .map_err(|error| error.message.clone())
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("racer joins"))
            .collect::<Vec<_>>()
    });
    unsafe {
        std::env::remove_var("PHASEGENT_DB_PATH");
    }
    let wins = first.iter().filter(|result| result.is_ok()).count();
    assert_eq!(wins, 1, "exactly one racer must win: {first:?}");
    for result in &first {
        if let Err(message) = result {
            assert!(
                message.contains("--isolate"),
                "the loser must get guidance, not raw SQLite text: {message}"
            );
        }
    }
    drop(temp);
}
