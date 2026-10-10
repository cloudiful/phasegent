use super::support::*;
use super::*;

#[test]
fn timer_ledger_is_additive_exact_and_finish_idempotent() {
    let (temp_dir, storage) = open_at_temp("timer-ledger");
    let columns = storage
        .connection
        .prepare("PRAGMA table_info(execution_timer_runs)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for expected in [
        "run_id",
        "issue_id",
        "phase",
        "role",
        "attempt",
        "started_at",
        "finished_at",
        "status",
        "elapsed_seconds",
        "rounded_hours",
        "activity_id",
        "redmine_time_entry_id",
        "sync_status",
    ] {
        assert!(columns.contains(&expected.to_owned()), "missing {expected}");
    }

    let run = storage
        .start_timer_run(
            "timer-run-1",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let duplicate = storage
        .start_timer_run(
            "timer-run-1",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    assert_eq!(run.run_id, duplicate.run_id);
    assert_eq!(run.started_at, duplicate.started_at);
    let count = storage
        .connection
        .query_row("SELECT count(*) FROM execution_timer_runs", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(count, 1, "duplicate start must not create another row");

    let finished = storage
        .finish_timer_run("timer-run-1", "DONE", 1_700_000_037)
        .unwrap();
    assert_eq!(finished.status, "DONE");
    assert_eq!(finished.elapsed_seconds, Some(37));
    assert_eq!(finished.rounded_hours, Some(0.02));
    let finished_again = storage
        .finish_timer_run("timer-run-1", "DONE", 1_700_000_038)
        .unwrap();
    assert_eq!(finished_again.time_entry_id, finished.time_entry_id);
    assert_eq!(finished_again.elapsed_seconds, Some(37));

    let reopened = Storage::open_at(&temp_dir.join(DB_FILENAME)).unwrap();
    let persisted = reopened.load_timer_run("timer-run-1").unwrap().unwrap();
    assert_eq!(persisted.status, "DONE");
    assert_eq!(persisted.elapsed_seconds, Some(37));
    assert_eq!(persisted.rounded_hours, Some(0.02));
    storage
        .start_timer_run(
            "same-second",
            28,
            "implementation",
            "executor",
            1,
            2_000_000_000,
        )
        .unwrap();
    let same_second = storage
        .finish_timer_run("same-second", "DONE", 2_000_000_000)
        .unwrap();
    assert_eq!(same_second.elapsed_seconds, Some(0));
    assert_eq!(same_second.rounded_hours, Some(0.01));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn timer_ledger_rejects_conflicting_identity_and_invalid_timestamps() {
    let (temp_dir, storage) = open_at_temp("timer-validation");
    storage
        .start_timer_run("timer-run-2", 28, "implementation", "reviewer", 1, 100)
        .unwrap();
    assert!(
        storage
            .start_timer_run("timer-run-2", 29, "implementation", "reviewer", 1, 100)
            .is_err()
    );
    assert!(storage.finish_timer_run("timer-run-2", "DONE", 99).is_err());
    assert!(storage.finish_timer_run("missing", "FAILED", 200).is_err());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn timer_ledger_keeps_historical_tester_rows_readable() {
    // A run recorded before the verification role merged into the reviewer
    // stays listable and finishable: the write path rejects the retired role
    // but no read re-validates the stored string, so the history needs no
    // database or credential edit.
    let (temp_dir, storage) = open_at_temp("timer-legacy-role");
    assert!(
        storage
            .start_timer_run("legacy", 28, "verification", "tester", 1, 100)
            .is_err()
    );
    storage
        .connection
        .execute(
            "INSERT INTO execution_timer_runs (run_id, issue_id, phase, role, attempt, started_at, status, sync_status) \
             VALUES ('legacy', 28, 'verification', 'tester', 1, 100, 'running', 'pending')",
            [],
        )
        .unwrap();
    let stored = storage.load_timer_run("legacy").unwrap().unwrap();
    assert_eq!(stored.role, "tester");
    let finished = storage.finish_timer_run("legacy", "DONE", 137).unwrap();
    assert_eq!(finished.status, "DONE");
    assert_eq!(finished.elapsed_seconds, Some(37));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn timer_ledger_distinguishes_synced_with_or_without_time_entry_id() {
    // A projection can advance sync_status to `synced` while leaving
    // `redmine_time_entry_id` null (no remote id was returned). The
    // Redmine path keeps its id-based behaviour so `load_timer_run`
    // always reports the actual state.
    let (temp_dir, storage) = open_at_temp("timer-synced-sync");
    let _ = storage
        .start_timer_run(
            "timer-synced",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let finished = storage
        .finish_timer_run("timer-synced", "DONE", 1_700_003_600)
        .unwrap();
    assert_eq!(finished.sync_status, "pending");
    assert!(finished.time_entry_id.is_none());

    let updated = storage
        .mark_timer_sync(
            "timer-synced",
            None,
            None,
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert_eq!(updated.sync_status, "synced");
    assert!(updated.time_entry_id.is_none());

    let persisted = storage.load_timer_run("timer-synced").unwrap().unwrap();
    assert_eq!(persisted.sync_status, "synced");
    assert!(persisted.time_entry_id.is_none());

    // The id-carrying path records the id and stays distinguishable
    // from the id-less path.
    let _ = storage
        .start_timer_run(
            "timer-redmine",
            8,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let _ = storage
        .finish_timer_run("timer-redmine", "DONE", 1_700_003_600)
        .unwrap();
    let redmine_synced = storage
        .mark_timer_sync(
            "timer-redmine",
            Some(11),
            Some(99),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    assert_eq!(redmine_synced.sync_status, "synced");
    assert_eq!(redmine_synced.time_entry_id, Some(99));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn timer_ledger_marks_failure_with_bounded_error_message() {
    // The failed-state recovery path records the bounded error so a retry
    // can see why the last projection failed.
    let (temp_dir, storage) = open_at_temp("timer-failed");
    let _ = storage
        .start_timer_run(
            "timer-fail",
            7,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let _ = storage
        .finish_timer_run("timer-fail", "DONE", 1_700_000_060)
        .unwrap();
    let updated = storage
        .mark_timer_sync(
            "timer-fail",
            None,
            None,
            crate::infra::storage::TIMER_SYNC_FAILED,
            Some("Redmine add time entry returned HTTP 422"),
        )
        .unwrap();
    assert_eq!(updated.sync_status, "failed");
    assert!(updated.sync_error.is_some());
    assert!(
        storage
            .mark_timer_sync(
                "timer-fail",
                None,
                None,
                crate::infra::storage::TIMER_SYNC_FAILED,
                None
            )
            .is_err()
    );
    let _ = fs::remove_dir_all(temp_dir);
}
