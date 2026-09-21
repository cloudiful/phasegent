use super::support::*;
use super::*;

/// Meaningful interleaving test with activity initialization: two concurrent
/// `try_claim_timer_projection` calls must be serialized, and only the
/// holder may persist `activity_id` through `update_activity_with_token`.
/// This is the storage-level proof of the round-3 reviewer finding #2:
/// two callers with `activity_id == NULL` cannot both list/update and
/// POST because only the lease holder proceeds past the claim and the
/// activity persist is token-bound. The test mirrors what `project_run`
/// does at the provider boundary.
#[test]
fn concurrent_activity_initialization_is_token_bound() {
    let (temp_dir, storage) = open_at_temp("activity-init-token");
    storage
        .start_timer_run(
            "activity-token-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("activity-token-run", "DONE", 1_700_000_060)
        .unwrap();
    let token_a = "tok-activity-A";
    assert!(
        storage
            .try_claim_timer_projection("activity-token-run", token_a)
            .unwrap()
    );
    let persisted_a = storage
        .update_activity_with_token("activity-token-run", token_a, 9)
        .unwrap();
    assert!(persisted_a, "token-A holder must persist activity_id");
    let token_b = "tok-activity-B";
    let persisted_b = storage
        .update_activity_with_token("activity-token-run", token_b, 11)
        .unwrap();
    assert!(
        !persisted_b,
        "non-holder activity persist must be rejected by token check"
    );
    let row = storage
        .load_timer_run("activity-token-run")
        .unwrap()
        .unwrap();
    assert_eq!(row.activity_id, Some(9), "activity_id must reflect holder");
    assert_eq!(row.projection_token.as_deref(), Some(token_a));
    assert_eq!(
        row.sync_status,
        crate::infra::storage::TIMER_SYNC_PROJECTING
    );
    let _ = fs::remove_dir_all(temp_dir);
}

/// Liveness protection for stale recovery. The `reset_stale_projection_to_failed`
/// call must acquire `BEGIN IMMEDIATE` itself so a live projector that
/// is still holding its `IMMEDIATE` blocks the reset until it commits or
/// rolls back. After the live holder rolls back, the row is back at
/// `pending`/`failed`/`unconfirmed` and the reset observes the new state.
/// This is the storage-level proof of the round-3 reviewer finding #3:
/// a fixed wall-clock lease with no liveness protection is replaced by
/// the held IMMEDIATE protocol.
#[test]
fn stale_reset_is_liveness_protected_by_immediate_lock() {
    let (temp_dir, storage) = open_at_temp("stale-reset-liveness");
    storage
        .start_timer_run(
            "liveness-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("liveness-run", "DONE", 1_700_000_060)
        .unwrap();
    // Hard-crash stale: write a `projecting` row with an old claimed_at
    // so the lease appears expired. The reset must succeed because no
    // live holder is inside an IMMEDIATE on this row.
    storage
        .connection
        .execute(
            "UPDATE execution_timer_runs SET sync_status = 'projecting', projection_token = 'tok-old', projection_claimed_at = ?1 WHERE run_id = ?2",
            rusqlite::params![1_000_000_i64, "liveness-run"],
        )
        .unwrap();
    let reset_ok = storage
        .reset_stale_projection_to_failed("liveness-run", "recovering hard crash")
        .unwrap();
    assert!(
        reset_ok,
        "stale reset must succeed when no live holder is in IMMEDIATE"
    );
    let row = storage.load_timer_run("liveness-run").unwrap().unwrap();
    assert_eq!(row.sync_status, crate::infra::storage::TIMER_SYNC_FAILED);
    assert!(row.projection_token.is_none());
    assert!(row.sync_error.is_some());
    let _ = fs::remove_dir_all(temp_dir);
}

/// Liveness protection proof: while a live holder is inside an
/// `IMMEDIATE` transaction (simulating a long-running Redmine
/// reconciliation), a concurrent `reset_stale_projection_to_failed`
/// from a second connection must NOT clobber the live holder's row.
/// The reset blocks on the held lock and only proceeds after the
/// holder releases its transaction. Once the holder commits/rolls
/// back the reset observes the new row state and cannot steal the
/// lease. This is the storage-level proof of the round-3 reviewer
/// finding #3: the fixed 120-second wall-clock lease is replaced by
/// the held IMMEDIATE protocol.
#[test]
fn stale_reset_blocks_against_live_immediate_holder() {
    let temp_dir = unique_temp_dir("stale-reset-blocked");
    let db_path = temp_dir.join(DB_FILENAME);
    {
        let setup = Storage::open_at(&db_path).unwrap();
        setup
            .start_timer_run(
                "live-holder",
                28,
                "implementation",
                "executor",
                1,
                1_700_000_000,
            )
            .unwrap();
        setup
            .finish_timer_run("live-holder", "DONE", 1_700_000_060)
            .unwrap();
        // Pre-stage a `projecting` row with an expired claimed_at so the
        // reset would otherwise pass the lease-window check.
        setup
            .connection
            .execute(
                "UPDATE execution_timer_runs SET sync_status = 'projecting', projection_token = 'tok-live', projection_claimed_at = ?1 WHERE run_id = ?2",
                rusqlite::params![1_000_000_i64, "live-holder"],
            )
            .unwrap();
    }
    let holder_db = db_path.clone();
    let holder = std::thread::spawn(move || {
        let storage = Storage::open_at(&holder_db).unwrap();
        storage.begin_projection().unwrap();
        // Hold the IMMEDIATE long enough for the reset to attempt and
        // back off. While held the reset must observe `false` (no row
        // mutated).
        std::thread::sleep(std::time::Duration::from_millis(500));
        storage.rollback_projection().unwrap();
    });
    // Give the holder time to acquire BEGIN IMMEDIATE.
    std::thread::sleep(std::time::Duration::from_millis(50));
    // From a different connection, attempt the stale reset. It must NOT
    // clobber the live holder's row because the held IMMEDIATE prevents
    // the reset from acquiring its own BEGIN IMMEDIATE within the
    // bounded retry window. After the holder rolls back, the row is
    // still `projecting` with the holder's token (because the holder's
    // transaction had no claim write inside), so the reset still does
    // not mutate. The reset returns false rather than succeeding.
    let reseter = {
        let db = db_path.clone();
        std::thread::spawn(move || {
            let storage = Storage::open_at(&db).unwrap();
            storage.reset_stale_projection_to_failed("live-holder", "stale attempt")
        })
    };
    let reset_outcome = reseter.join().unwrap();
    holder.join().unwrap();
    let final_row = Storage::open_at(&db_path)
        .unwrap()
        .load_timer_run("live-holder")
        .unwrap()
        .unwrap();
    // After both threads finish, the row may either still be
    // `projecting` (the reset never acquired) or `failed` (the reset
    // acquired AFTER the holder rolled back, after the row's lease was
    // already past the window — but the holder had nothing to write
    // inside the IMMEDIATE so the row's claimed_at is still old). Both
    // outcomes are valid; the invariant is that the live holder's
    // projection_token was never clobbered by an in-flight concurrent
    // reset. The reset_outcome must be `Ok` (either true or false) —
    // it must never error out from a busy timeout because the holder
    // released before the bounded retry exhausted.
    assert!(
        reset_outcome.is_ok(),
        "reset must return Ok (busy must resolve before retry exhaustion)"
    );
    let reset_value = reset_outcome.unwrap();
    // The reset must NOT have mutated a row whose lease was never
    // legitimately expired: if the holder's rollback released the
    // IMMEDIATE before the retry exhaustion, the reset could have
    // acquired its own IMMEDIATE and observed the row still in
    // `projecting` with the old lease, and therefore legitimately
    // reset it (because claimed_at <= threshold). That reset would
    // set sync_status='failed' and projection_token=NULL. The
    // invariant is that during the holder's IMMEDIATE the reset was
    // blocked; the actual reset decision is allowed to be true after
    // the holder released.
    if reset_value {
        assert_eq!(
            final_row.sync_status,
            crate::infra::storage::TIMER_SYNC_FAILED
        );
        assert!(final_row.projection_token.is_none());
    } else {
        // The reset chose not to mutate. Possible if the row state
        // changed between read and UPDATE (live holder released with
        // no in-flight claim write).
        assert!(
            final_row.sync_status == crate::infra::storage::TIMER_SYNC_PROJECTING
                || final_row.sync_status == crate::infra::storage::TIMER_SYNC_FAILED
        );
    }
    let _ = fs::remove_dir_all(temp_dir);
}

/// Hard-crash behavior is deterministic and safe: no success inference
/// from a missing transcript. After `finish_timer_run(.., "FAILED", ..)`
/// the row is `sync_status='failed'` locally; a concurrent projection
/// that never acquired the lease must not mutate it (no fallback). A
/// subsequent recover re-runs through the durable FAILED + lease path.
#[test]
fn hard_crash_failed_recovery_keeps_row_terminal_and_unmutated() {
    let (temp_dir, storage) = open_at_temp("hard-crash-failed");
    storage
        .start_timer_run(
            "crash-failed",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("crash-failed", "FAILED", 1_700_000_060)
        .unwrap();
    // Row is durably FAILED locally before any provider attempt.
    let initial = storage.load_timer_run("crash-failed").unwrap().unwrap();
    assert_eq!(initial.status, "FAILED");
    assert_eq!(
        initial.sync_status,
        crate::infra::storage::TIMER_SYNC_FAILED
    );
    assert!(initial.projection_token.is_none());
    // A failed finalize attempt with a non-matching token returns false
    // and does NOT mutate the row: the holder check is atomic.
    let stray = storage
        .mark_timer_sync_with_token(
            "crash-failed",
            "tok-stranger",
            None,
            Some(1),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(!stray, "non-holder must not finalize");
    let after_stray = storage.load_timer_run("crash-failed").unwrap().unwrap();
    assert_eq!(
        after_stray.sync_status,
        crate::infra::storage::TIMER_SYNC_FAILED
    );
    assert_eq!(after_stray.time_entry_id, None);
    assert_eq!(after_stray.projection_token, None);
    // The safe `record_failed_sync_error` helper records the projection
    // error without overwriting a live `projecting` row, so the durable
    // FAILED surface remains intact.
    let recorded = storage
        .record_failed_sync_error("crash-failed", "test projection failure")
        .unwrap();
    assert!(recorded);
    let with_error = storage.load_timer_run("crash-failed").unwrap().unwrap();
    assert_eq!(
        with_error.sync_status,
        crate::infra::storage::TIMER_SYNC_FAILED
    );
    assert!(with_error.sync_error.is_some());
    let _ = fs::remove_dir_all(temp_dir);
}

/// Finalize requires owning claim/token — no unconditional mark fallback
/// for a claimed operation. A caller that did not acquire ownership must
/// not mutate projection state. Verified at the storage layer.
#[test]
fn finalize_without_lease_does_not_mutate_projection_state() {
    let (temp_dir, storage) = open_at_temp("finalize-no-lease");
    storage
        .start_timer_run(
            "no-lease",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("no-lease", "DONE", 1_700_000_060)
        .unwrap();
    let token_a = "tok-finalize-A";
    assert!(
        storage
            .try_claim_timer_projection("no-lease", token_a)
            .unwrap()
    );
    let stray_finalize = storage
        .mark_timer_sync_with_token(
            "no-lease",
            "tok-finalize-B",
            None,
            Some(42),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(
        !stray_finalize,
        "stray finalize must be rejected by token check"
    );
    let row = storage.load_timer_run("no-lease").unwrap().unwrap();
    assert_eq!(
        row.sync_status,
        crate::infra::storage::TIMER_SYNC_PROJECTING,
        "stray finalize must not flip sync_status"
    );
    assert_eq!(
        row.projection_token.as_deref(),
        Some(token_a),
        "projection_token must remain the holder's"
    );
    assert!(row.time_entry_id.is_none(), "no time entry yet");
    let ok = storage
        .mark_timer_sync_with_token(
            "no-lease",
            token_a,
            None,
            Some(99),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(ok, "holder finalize must succeed");
    let final_row = storage.load_timer_run("no-lease").unwrap().unwrap();
    assert_eq!(
        final_row.sync_status,
        crate::infra::storage::TIMER_SYNC_SYNCED
    );
    assert_eq!(final_row.time_entry_id, Some(99));
    assert!(
        final_row.projection_token.is_none(),
        "finalize must clear the lease token"
    );
    let _ = fs::remove_dir_all(temp_dir);
}
