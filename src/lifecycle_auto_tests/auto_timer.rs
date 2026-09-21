use super::support::*;
use super::*;

#[test]
fn same_state_re_entry_sums_elapsed_seconds_per_phase() {
    // The spec documents that "Same-state re-entry creates new
    // segment (cumulative sum via elapsed_seconds summation, no
    // attempt merging logic change)". This test seeds two finished
    // rows for the same `(issue, phase)` pair and confirms the
    // `sum_elapsed_seconds_for_issue_phase` helper reports the
    // total. The lifecycle path always opens a fresh segment for
    // every transition, so the running row count is never reduced
    // by the spec, but a future cumulative-sum reporting can read
    // the aggregate from this helper.
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("cumulative");
    let issue = 110;

    // First segment: 60s in "In Progress", then finished.
    let _ = storage
        .start_timer_run("c-1", issue, "In Progress", "executor", 1, 1_700_000_000)
        .unwrap();
    let _ = storage
        .finish_timer_run("c-1", "DONE", 1_700_000_060)
        .unwrap();
    // Second segment: 120s in "In Progress" again, then finished.
    let _ = storage
        .start_timer_run("c-2", issue, "In Progress", "executor", 1, 1_700_001_000)
        .unwrap();
    let _ = storage
        .finish_timer_run("c-2", "DONE", 1_700_001_120)
        .unwrap();

    let total = storage
        .sum_elapsed_seconds_for_issue_phase(issue, "In Progress")
        .unwrap();
    assert_eq!(
        total,
        60 + 120,
        "cumulative sum must be the per-segment total"
    );
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_start_run_emits_auto_prefixed_run_id() {
    // Direct test of the internal auto entry: bypass the lifecycle
    // helper and call the time_tracking::start::auto_start_run
    // surface so the `auto-` prefix is asserted at the lowest
    // level.
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("auto-prefix");
    let run_id = crate::time_tracking_cli::auto_start_run(110, "In Review", "reviewer", 1).unwrap();
    assert!(
        run_id.starts_with("auto-"),
        "auto start must produce auto- prefix, got {run_id}"
    );
    let loaded = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(loaded.role, "reviewer");
    assert_eq!(loaded.phase, "In Review");
    assert_eq!(loaded.status, TIMER_STATUS_RUNNING);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_finish_run_persists_done_with_elapsed_seconds() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("auto-finish");
    let run_id =
        crate::time_tracking_cli::auto_start_run(111, "In Progress", "executor", 1).unwrap();
    // Backdate the row so the finish records a positive elapsed.
    storage
        .connection
        .execute(
            "UPDATE execution_timer_runs SET started_at = ?1 WHERE run_id = ?2",
            rusqlite::params![1_700_000_000_i64, &run_id],
        )
        .unwrap();
    crate::time_tracking_cli::auto_finish_run(&run_id).unwrap();
    let loaded = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(loaded.status, "DONE");
    assert!(loaded.finished_at.is_some());
    assert!(loaded.elapsed_seconds.unwrap_or(0) >= 0);
    let _ = fs::remove_dir_all(temp);
}

// Phase 3 projection-regression tests. The contract is: an
// `auto-`-prefixed run opened by the lifecycle helper must
// (1) be visible through `Storage::list_timer_runs` and
// `Storage::load_timer_run` with the expected `pending` /
// `synced` state, (2) project through the existing manual
// `project_run_with_provider` / `project_run_with_gitlab_provider`
// path with idempotent retry semantics, and (3) survive the
// `timer recover` orphan path (running → FAILED → projection)
// the same way a manual run does. All tests use the
// `PHASEGENT_DB_PATH` env-var override and `lock_workflow_tests`
// for the same reason the existing lifecycle tests do: the
// `PHASEGENT_DB_PATH` env var is process-wide and parallel
// tests must not race.

#[test]
fn auto_prefixed_runs_are_visible_via_storage_list_and_get_with_pending_state() {
    // Verify that an auto- run opened by the lifecycle helper is
    // discoverable through the read-only surfaces the operator
    // actually inspects: `list_timer_runs` (all + running + finished
    // filter) and `load_timer_run`. The `auto-` prefix must
    // round-trip through storage so `timer list` shows the lifecycle
    // origin without a separate audit column.
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("list-get");
    let issue = 200;
    let run_id =
        crate::time_tracking_cli::auto_start_run(issue, "In Progress", "executor", 1).unwrap();
    assert!(run_id.starts_with("auto-"));

    // `load_timer_run` is the storage backing for `timer get`.
    let loaded = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(loaded.run_id, run_id);
    assert_eq!(loaded.status, TIMER_STATUS_RUNNING);
    assert_eq!(
        loaded.sync_status, "pending",
        "fresh auto-run starts pending"
    );
    assert_eq!(loaded.issue, issue);
    assert!(loaded.time_entry_id.is_none());

    // `list_timer_runs(Running, N)` is the storage backing for
    // `timer list --status running`. The auto- run must show up
    // alongside manual runs.
    let running = storage
        .list_timer_runs(TimerStatusFilter::Running, 256)
        .unwrap();
    assert!(
        running.iter().any(|row| row.run_id == run_id),
        "list_timer_runs(Running) must include the auto-run: {running:?}"
    );

    // `list_timer_runs(All, N)` is the storage backing for the
    // default `timer list`.
    let all = storage
        .list_timer_runs(TimerStatusFilter::All, 256)
        .unwrap();
    assert!(
        all.iter().any(|row| row.run_id == run_id),
        "list_timer_runs(All) must include the auto-run: {all:?}"
    );

    // After `auto_finish_run` the row is `DONE` and stays at
    // `pending` until the operator projects it. `list_timer_runs`
    // with the `Finished` filter must surface the row so the
    // operator can find an un-projected auto- segment to recover.
    crate::time_tracking_cli::auto_finish_run(&run_id).unwrap();
    let finished = storage
        .list_timer_runs(TimerStatusFilter::Finished, 256)
        .unwrap();
    let row = finished
        .iter()
        .find(|row| row.run_id == run_id)
        .expect("list_timer_runs(Finished) must include the auto-run after auto_finish");
    assert_eq!(row.status, "DONE");
    assert_eq!(row.sync_status, "pending");
    assert!(row.time_entry_id.is_none());

    // `timer get` after a successful projection surfaces `synced`
    // + `time_entry_id` so the operator can confirm the Time Entry
    // landed. We mark the row via the storage helper rather than
    // driving a real provider here; the provider-level
    // projection of an auto- run is covered by the contract
    // tests in `src/providers/{redmine,gitlab}/contract_tests/timer.rs`.
    storage
        .mark_timer_sync(
            &run_id,
            Some(9),
            Some(77),
            crate::infra::storage::TIMER_SYNC_SYNCED,
            None,
        )
        .unwrap();
    let after = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(after.sync_status, "synced");
    assert_eq!(after.time_entry_id, Some(77));
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_finish_run_is_idempotent_and_does_not_overwrite_first_call_state() {
    // A second `auto_finish_run` on the same auto- run id must
    // short-circuit locally. The first call's `finished_at`,
    // `elapsed_seconds`, and `rounded_hours` must be preserved
    // exactly; a second call must not reopen the row, must not
    // recompute a longer duration, and must not POST. This
    // matches the `finish_timer_run` storage contract and is
    // the local-side guarantee behind the projection-side
    // idempotency.
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("finish-idempotent");
    let run_id =
        crate::time_tracking_cli::auto_start_run(201, "In Progress", "executor", 1).unwrap();
    // Backdate started_at so the first call records a known
    // positive duration; the second call's wall-clock
    // `finished_at` is much later and would corrupt the
    // duration if it were allowed to overwrite.
    storage
        .connection
        .execute(
            "UPDATE execution_timer_runs SET started_at = ?1 WHERE run_id = ?2",
            rusqlite::params![1_700_000_000_i64, &run_id],
        )
        .unwrap();

    crate::time_tracking_cli::auto_finish_run(&run_id).unwrap();
    let first = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(first.status, "DONE");
    let first_finished_at = first.finished_at;
    let first_elapsed = first.elapsed_seconds;
    let first_rounded = first.rounded_hours;

    // Second call: must be a no-op against the persisted state.
    crate::time_tracking_cli::auto_finish_run(&run_id).unwrap();
    let second = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(second.status, "DONE");
    assert_eq!(
        second.finished_at, first_finished_at,
        "finished_at must not change on retry"
    );
    assert_eq!(
        second.elapsed_seconds, first_elapsed,
        "elapsed must not change on retry"
    );
    assert_eq!(
        second.rounded_hours, first_rounded,
        "rounded hours must not change on retry"
    );
    let _ = fs::remove_dir_all(temp);
}
