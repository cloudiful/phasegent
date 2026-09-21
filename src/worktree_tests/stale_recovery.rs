use super::support::*;
use super::*;

#[test]
fn stale_scan_lists_only_aged_active_leases() {
    let _lock = lock_workflow_tests();
    let (db_temp, storage, _env) = open_temp_db("stale-dry");
    crate::worktree::ensure_schema(&storage).expect("schema");
    let now = now_unix_secs();
    let old = now - 30 * 86_400;
    let recent = now - 60;
    insert_lease_row(
        &storage,
        "stale-old",
        "/tmp/stale-repo",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/wt-old",
    );
    insert_lease_row(
        &storage,
        "stale-recent",
        "/tmp/stale-repo",
        LEASE_STATUS_ACTIVE,
        recent,
        "/tmp/wt-recent",
    );
    insert_lease_row(
        &storage,
        "stale-retained",
        "/tmp/stale-repo",
        LEASE_STATUS_RETAINED,
        old,
        "/tmp/wt-retained",
    );
    let stale_before = now - 14 * 86_400;
    let candidates = stale_active_leases(&storage, "/tmp/stale-repo", stale_before).expect("scan");
    let ids: Vec<&str> = candidates.iter().map(|row| row.lease_id.as_str()).collect();
    assert_eq!(ids, vec!["stale-old"]);
    assert_eq!(candidates[0].status, LEASE_STATUS_ACTIVE);
    let after = leases_for_repo("/tmp/stale-repo").expect("list");
    let old_row = after
        .iter()
        .find(|row| row.lease_id == "stale-old")
        .expect("row");
    assert_eq!(old_row.status, LEASE_STATUS_ACTIVE, "scan must not mutate");
    assert!(old_row.release_reason.is_none());
    drop(db_temp);
}

#[test]
fn stale_recovery_apply_flips_candidates_and_keeps_worktree() {
    let _lock = lock_workflow_tests();
    let (db_temp, mut storage, _env) = open_temp_db("stale-apply");
    crate::worktree::ensure_schema(&storage).expect("schema");
    let now = now_unix_secs();
    let old = now - 30 * 86_400;
    let recent = now - 60;
    let worktree = unique_cache("stale-apply-wt");
    let worktree_path = worktree.path().to_string_lossy().to_string();
    insert_lease_row(
        &storage,
        "stale-flip",
        "/tmp/stale-repo",
        LEASE_STATUS_ACTIVE,
        old,
        &worktree_path,
    );
    insert_lease_row(
        &storage,
        "stale-keep",
        "/tmp/stale-repo",
        LEASE_STATUS_ACTIVE,
        recent,
        "/tmp/wt-keep",
    );
    let stale_before = now - 14 * 86_400;
    let flipped = recover_stale_active_leases(
        &mut storage,
        "/tmp/stale-repo",
        stale_before,
        "stale session recovery",
        now,
    )
    .expect("apply");
    assert_eq!(flipped.len(), 1);
    assert_eq!(flipped[0].lease_id, "stale-flip");
    assert_eq!(flipped[0].status, LEASE_STATUS_RETAINED);
    assert_eq!(
        flipped[0].release_reason.as_deref(),
        Some("stale session recovery")
    );
    let rows = leases_for_repo("/tmp/stale-repo").expect("list");
    let flipped_row = rows
        .iter()
        .find(|row| row.lease_id == "stale-flip")
        .expect("flipped row");
    assert_eq!(flipped_row.status, LEASE_STATUS_RETAINED);
    assert_eq!(
        flipped_row.release_reason.as_deref(),
        Some("stale session recovery")
    );
    let kept_row = rows
        .iter()
        .find(|row| row.lease_id == "stale-keep")
        .expect("kept row");
    assert_eq!(kept_row.status, LEASE_STATUS_ACTIVE);
    assert!(kept_row.release_reason.is_none());
    assert!(
        worktree.path().exists(),
        "stale recovery must never delete the worktree directory"
    );
    drop(worktree);
    drop(db_temp);
}

#[test]
fn stale_recovery_rejects_blank_reason() {
    let _lock = lock_workflow_tests();
    let (db_temp, mut storage, _env) = open_temp_db("stale-reason");
    crate::worktree::ensure_schema(&storage).expect("schema");
    let now = now_unix_secs();
    let old = now - 30 * 86_400;
    insert_lease_row(
        &storage,
        "stale-blank",
        "/tmp/stale-repo",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/wt-blank",
    );
    let error = recover_stale_active_leases(
        &mut storage,
        "/tmp/stale-repo",
        now - 14 * 86_400,
        "   ",
        now,
    )
    .expect_err("blank reason must be rejected");
    assert_eq!(error.kind, "argument");
    let row = leases_for_repo("/tmp/stale-repo")
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "stale-blank")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_ACTIVE);
    assert!(row.release_reason.is_none());
    drop(db_temp);
}

#[test]
fn stale_recovery_is_scoped_to_repo_identity() {
    let _lock = lock_workflow_tests();
    let (db_temp, mut storage, _env) = open_temp_db("stale-scope");
    crate::worktree::ensure_schema(&storage).expect("schema");
    let now = now_unix_secs();
    let old = now - 30 * 86_400;
    insert_lease_row(
        &storage,
        "stale-a",
        "/tmp/repo-a",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/wt-a",
    );
    insert_lease_row(
        &storage,
        "stale-b",
        "/tmp/repo-b",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/wt-b",
    );
    let flipped = recover_stale_active_leases(
        &mut storage,
        "/tmp/repo-a",
        now - 14 * 86_400,
        "repo a only",
        now,
    )
    .expect("apply");
    assert_eq!(flipped.len(), 1);
    let other = leases_for_repo("/tmp/repo-b")
        .expect("list b")
        .into_iter()
        .find(|row| row.lease_id == "stale-b")
        .expect("row b");
    assert_eq!(other.status, LEASE_STATUS_ACTIVE, "other repos stay active");
    assert!(other.release_reason.is_none());
    drop(db_temp);
}

#[test]
fn concurrent_stale_recovery_flips_each_lease_at_most_once() {
    let _lock = lock_workflow_tests();
    let (db_temp, storage, _env) = open_temp_db("stale-race");
    crate::worktree::ensure_schema(&storage).expect("schema");
    let now = now_unix_secs();
    let old = now - 40 * 86_400;
    for index in 0..4 {
        insert_lease_row(
            &storage,
            &format!("race-{index}"),
            "/tmp/race-repo",
            LEASE_STATUS_ACTIVE,
            old,
            &format!("/tmp/wt-race-{index}"),
        );
    }
    let stale_before = now - 14 * 86_400;
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(move || {
                let mut storage =
                    Storage::open().map_err(|error| WorktreeError::new("storage", error))?;
                crate::worktree::ensure_schema(&storage)
                    .map_err(|error| WorktreeError::new("storage", error))?;
                recover_stale_active_leases(
                    &mut storage,
                    "/tmp/race-repo",
                    stale_before,
                    "race recovery",
                    now_unix_secs(),
                )
                .map(|rows| rows.len())
            })
        })
        .collect();
    let mut total = 0usize;
    for handle in handles {
        total += handle
            .join()
            .expect("racer thread panicked")
            .expect("racer must not hit a storage error");
    }
    assert_eq!(
        total, 4,
        "each stale lease must flip exactly once across concurrent racers"
    );
    let rows = leases_for_repo("/tmp/race-repo").expect("list");
    assert_eq!(rows.len(), 4);
    assert!(
        rows.iter().all(|row| row.status == LEASE_STATUS_RETAINED),
        "every stale lease must end retained"
    );
    drop(db_temp);
}

#[test]
fn stale_recovery_rechecks_the_cutoff_after_a_heartbeat() {
    let _lock = lock_workflow_tests();
    let (db_temp, mut storage, _env) = open_temp_db("stale-recheck");
    crate::worktree::ensure_schema(&storage).expect("schema");
    let now = now_unix_secs();
    let old = now - 30 * 86_400;
    insert_lease_row(
        &storage,
        "stale-recheck",
        "/tmp/recheck-repo",
        LEASE_STATUS_ACTIVE,
        old,
        "/tmp/wt-recheck",
    );
    let stale_before = now - 14 * 86_400;
    // The read-only scan reports the aged active lease as a candidate.
    let candidates =
        stale_active_leases(&storage, "/tmp/recheck-repo", stale_before).expect("scan");
    assert_eq!(candidates.len(), 1);
    // A heartbeat from the owning session lands between the scan and the
    // apply, so the transactional recovery must re-check the predicate
    // and leave the row active instead of clobbering the fresh heartbeat.
    assert!(
        heartbeat_active_lease(&storage, "stale-recheck", "session-A", now + 1).expect("heartbeat")
    );
    let flipped = recover_stale_active_leases(
        &mut storage,
        "/tmp/recheck-repo",
        stale_before,
        "race recovery",
        now + 2,
    )
    .expect("apply");
    assert!(
        flipped.is_empty(),
        "a freshly heartbeated lease must not flip"
    );
    let row = leases_for_repo("/tmp/recheck-repo")
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "stale-recheck")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_ACTIVE);
    assert!(row.release_reason.is_none());
    drop(db_temp);
}
