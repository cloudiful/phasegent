use super::support::*;
use super::*;

#[test]
fn concurrent_projection_claim_is_serialized() {
    let (temp_dir, storage) = open_at_temp("projection-claim");
    storage
        .start_timer_run(
            "claim-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("claim-run", "FAILED", 1_700_000_060)
        .unwrap();
    // Two concurrent claim attempts with distinct caller-bound tokens: only
    // one may move pending->projecting. The token binds the lease to the
    // caller so a second concurrent finish cannot reuse the loaded
    // projecting row.
    let path = temp_dir.join(DB_FILENAME);
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let p = path.clone();
            std::thread::spawn(move || {
                let s = Storage::open_at(&p).unwrap();
                let token = format!("tok-claim-{i}-{}", std::process::id());
                s.try_claim_timer_projection("claim-run", &token).unwrap()
            })
        })
        .collect();
    let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        results.iter().filter(|&&v| v).count(),
        1,
        "only one concurrent claim may succeed"
    );
    let final_row = Storage::open_at(&path)
        .unwrap()
        .load_timer_run("claim-run")
        .unwrap()
        .unwrap();
    assert_eq!(
        final_row.sync_status,
        crate::infra::storage::TIMER_SYNC_PROJECTING
    );
    assert!(final_row.projection_token.is_some());
    assert!(final_row.projection_claimed_at.is_some());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn projection_lease_token_binds_finalization_and_prevents_second_post() {
    // Regression: a second finish that loads a terminal projecting row
    // must not be treated as the owner. Only the holder of the token may
    // finalize to synced; a concurrent caller with a different token must
    // see "projection already in progress" and never POST.
    let (temp_dir, storage) = open_at_temp("projection-ownership");
    storage
        .start_timer_run(
            "owner-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    storage
        .finish_timer_run("owner-run", "DONE", 1_700_000_060)
        .unwrap();
    let token_a = "tok-owner-A";
    assert!(
        storage
            .try_claim_timer_projection("owner-run", token_a)
            .unwrap()
    );
    let claimed = storage.load_timer_run("owner-run").unwrap().unwrap();
    assert_eq!(claimed.projection_token.as_deref(), Some(token_a));
    assert_eq!(
        claimed.sync_status,
        crate::infra::storage::TIMER_SYNC_PROJECTING
    );
    let token_b = "tok-owner-B";
    assert!(
        !storage
            .try_claim_timer_projection("owner-run", token_b)
            .unwrap()
    );
    let marked_b = storage
        .mark_timer_sync_with_token(
            "owner-run",
            token_b,
            None,
            Some(999),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(
        !marked_b,
        "second caller must not finalize with wrong token"
    );
    let marked_a = storage
        .mark_timer_sync_with_token(
            "owner-run",
            token_a,
            None,
            Some(1001),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(marked_a, "holder must be able to finalize");
    let final_row = storage.load_timer_run("owner-run").unwrap().unwrap();
    assert_eq!(
        final_row.sync_status,
        crate::infra::storage::TIMER_SYNC_SYNCED
    );
    assert_eq!(final_row.time_entry_id, Some(1001));
    assert!(final_row.projection_token.is_none());
    // A stale reset must not clear a live lease. Simulate a concurrent
    // recover trying to force-reset while the lease is still fresh.
    // Create a fresh projecting row with recent claimed_at.
    storage
        .start_timer_run(
            "stale-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_100,
        )
        .unwrap();
    storage
        .finish_timer_run("stale-run", "DONE", 1_700_000_160)
        .unwrap();
    let live_token = "tok-live";
    assert!(
        storage
            .try_claim_timer_projection("stale-run", live_token)
            .unwrap()
    );
    let stale_reset = storage
        .reset_stale_projection_to_failed("stale-run", "stale")
        .unwrap();
    assert!(
        !stale_reset,
        "live lease must not be reset as stale within window"
    );
    let marked_live = storage
        .mark_timer_sync_with_token(
            "stale-run",
            live_token,
            None,
            Some(2002),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert!(marked_live);
    // Simulate hard-crash stale: insert a projecting row with old timestamp
    // directly via SQL so the lease appears expired.
    let stale_id = "hard-crash-run";
    storage
        .start_timer_run(stale_id, 28, "implementation", "executor", 1, 1_700_000_200)
        .unwrap();
    storage
        .finish_timer_run(stale_id, "DONE", 1_700_000_260)
        .unwrap();
    storage
        .connection
        .execute(
            "UPDATE execution_timer_runs SET sync_status = 'projecting', projection_token = 'tok-old', projection_claimed_at = ?1 WHERE run_id = ?2",
            rusqlite::params![1_000_000_i64, stale_id],
        )
        .unwrap();
    let stale_ok = storage
        .reset_stale_projection_to_failed(stale_id, "recovering hard crash")
        .unwrap();
    assert!(stale_ok, "expired lease must be recoverable");
    let after = storage.load_timer_run(stale_id).unwrap().unwrap();
    assert_eq!(after.sync_status, crate::infra::storage::TIMER_SYNC_FAILED);
    assert!(after.projection_token.is_none());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn legacy_owner_migration_tolerates_concurrent_opens() {
    // Simulate a pre-owner database by creating the legacy schema without
    // owner columns, then opening it concurrently from two threads.
    let temp_dir = unique_temp_dir("legacy-concurrent");
    fs::create_dir_all(&temp_dir).unwrap();
    let db_path = temp_dir.join(DB_FILENAME);
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS execution_timer_runs (
                 run_id TEXT PRIMARY KEY, issue_id INTEGER NOT NULL, phase TEXT NOT NULL,
                 role TEXT NOT NULL, attempt INTEGER NOT NULL, started_at INTEGER NOT NULL,
                 finished_at INTEGER, status TEXT NOT NULL, elapsed_seconds INTEGER,
                 rounded_hours REAL, activity_id INTEGER, redmine_time_entry_id INTEGER,
                 sync_status TEXT NOT NULL DEFAULT 'pending', sync_error TEXT
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO execution_timer_runs (run_id, issue_id, phase, role, attempt, started_at, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["legacy-1", 28, "implementation", "executor", 1, 1_700_000_000, "running"],
        )
        .unwrap();
    }
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let p = db_path.clone();
            std::thread::spawn(move || Storage::open_at(&p))
        })
        .collect();
    for h in handles {
        let storage = h.join().unwrap().unwrap();
        let row = storage.load_timer_run("legacy-1").unwrap().unwrap();
        assert_eq!(row.run_id, "legacy-1");
        assert!(row.owner_session_id.is_none());
    }
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .start_timer_run_with_owner(
            "new-after-legacy",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_001,
            &TimerRunOwner {
                session_id: Some("sess-x".to_owned()),
                call_id: Some("call-y".to_owned()),
            },
        )
        .unwrap();
    let _ = fs::remove_dir_all(temp_dir);
}
