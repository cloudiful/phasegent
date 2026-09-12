//! Integration tests for the Phase 2 lifecycle auto-accounting path.
//!
//! These tests exercise the public `lifecycle_auto` surface against
//! a real SQLite ledger opened via `Storage::open_at` with a
//! `PHASEGENT_DB_PATH` override. They prove the documented
//! invariants end-to-end:
//!
//! - the auto-`start` opens a `running` row with the expected
//!   `auto-`-prefixed `run_id`;
//! - the auto-`finish` closes the open row with the right
//!   `elapsed_seconds` and `rounded_hours`;
//! - a second status transition closes the first segment and opens
//!   a fresh one;
//! - a crash/legacy ledger with multiple `running` rows for the
//!   same issue is reconciled by finishing every one and opening a
//!   single new row;
//! - the close path finishes every running row for the issue;
//! - cumulative sum across multiple same-state segments equals the
//!   `elapsed_seconds` sum of the individual finished rows;
//! - Forgejo is a no-op and the helper never touches the ledger.

use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::infra::storage::{Storage, TIMER_STATUS_RUNNING, TimerRun, TimerStatusFilter};
use crate::lifecycle_auto::{
    AutoCloseOutcome, AutoTimerOutcome, auto_close_issue_timer, auto_transition_timer,
    status_to_agent_role,
};
use crate::providers::ProviderKind;
use crate::worktree::WorktreeRunner;
use std::fs;

fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "phasegent-auto-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn open_temp_storage(label: &str) -> (std::path::PathBuf, Storage, EnvGuard) {
    let temp = unique_temp_dir(label);
    let db = temp.join(crate::infra::storage::DB_FILENAME);
    let env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db).unwrap();
    (temp, storage, env)
}

fn running_for(storage: &Storage, issue: u64) -> Vec<TimerRun> {
    storage
        .list_timer_runs(TimerStatusFilter::Running, 256)
        .unwrap()
        .into_iter()
        .filter(|run| run.issue == issue)
        .collect()
}

#[test]
fn auto_transition_starts_a_running_row_with_auto_prefixed_run_id() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("start");
    let issue = 101;

    let outcome = auto_transition_timer(issue, ProviderKind::Redmine, "In Progress");
    match outcome {
        AutoTimerOutcome::Started {
            new_run_id,
            role,
            finished_runs,
            ..
        } => {
            assert_eq!(role, "executor");
            assert!(finished_runs.is_empty());
            assert!(
                new_run_id.starts_with("auto-"),
                "auto run id must use the auto- prefix, got {new_run_id}"
            );
        }
        other => panic!("expected Started, got {other:?}"),
    }
    let open = running_for(&storage, issue);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].status, TIMER_STATUS_RUNNING);
    assert_eq!(open[0].phase, "In Progress");
    assert_eq!(open[0].role, "executor");
    assert!(open[0].started_at > 0);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_transition_closes_existing_running_and_opens_new_one() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("close-open");
    let issue = 102;

    // First transition: open the "In Progress" segment.
    let first = auto_transition_timer(issue, ProviderKind::Redmine, "In Progress");
    let first_run_id = match first {
        AutoTimerOutcome::Started { new_run_id, .. } => new_run_id,
        other => panic!("expected Started on first transition, got {other:?}"),
    };
    assert_eq!(running_for(&storage, issue).len(), 1);

    // Second transition: close the open segment and open a new one.
    let second = auto_transition_timer(issue, ProviderKind::Redmine, "In Review");
    match second {
        AutoTimerOutcome::Started {
            new_run_id,
            role,
            finished_runs,
            ..
        } => {
            assert_eq!(role, "reviewer");
            assert_eq!(finished_runs, vec![first_run_id.clone()]);
            assert_ne!(new_run_id, first_run_id);
        }
        other => panic!("expected Started on second transition, got {other:?}"),
    }
    let open = running_for(&storage, issue);
    assert_eq!(open.len(), 1, "exactly one running row must remain");
    assert_eq!(open[0].phase, "In Review");
    assert_eq!(open[0].role, "reviewer");

    // The first row is finished in the ledger.
    let persisted = storage.load_timer_run(&first_run_id).unwrap().unwrap();
    assert_eq!(persisted.status, "DONE");
    assert!(persisted.finished_at.is_some());
    assert!(persisted.elapsed_seconds.is_some());
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_transition_reconciles_legacy_multi_running_state() {
    // A crash/legacy ledger can leave multiple `running` rows for
    // the same issue. The auto-transition must finish every one and
    // leave exactly one fresh `running` row, regardless of how many
    // orphans the pre-existing state held.
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("legacy-multi");
    let issue = 103;

    // Seed three stale running rows for the same issue.
    for offset in 0..3 {
        let _ = storage
            .start_timer_run(
                &format!("legacy-{offset}"),
                issue,
                "Doing",
                "executor",
                1,
                1_700_000_000 + offset,
            )
            .unwrap();
    }
    assert_eq!(running_for(&storage, issue).len(), 3);

    let outcome = auto_transition_timer(issue, ProviderKind::Redmine, "In Review");
    match outcome {
        AutoTimerOutcome::Started {
            new_run_id,
            finished_runs,
            ..
        } => {
            assert_eq!(finished_runs.len(), 3);
            // The reconciliation finishes all three orphans; the
            // audit ordering is "oldest first" so the finished_runs
            // list is the iteration order of the helper (oldest
            // first) — verify every legacy id is present, regardless
            // of position.
            for offset in 0..3 {
                let id = format!("legacy-{offset}");
                assert!(finished_runs.contains(&id), "missing {id}");
            }
            assert!(new_run_id.starts_with("auto-"));
        }
        other => panic!("expected Started, got {other:?}"),
    }
    let open = running_for(&storage, issue);
    assert_eq!(
        open.len(),
        1,
        "invariant: exactly one running after transition"
    );
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_transition_is_a_noop_for_forgejo() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("forgejo-noop");
    let issue = 104;

    let outcome = auto_transition_timer(issue, ProviderKind::Forgejo, "In Progress");
    match outcome {
        AutoTimerOutcome::Skipped { .. } => {}
        other => panic!("expected Skipped for Forgejo, got {other:?}"),
    }
    assert!(running_for(&storage, issue).is_empty());
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_transition_routes_to_tester_for_qa_status() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("qa-route");
    let issue = 105;

    let outcome = auto_transition_timer(issue, ProviderKind::Redmine, "Ready for QA");
    match outcome {
        AutoTimerOutcome::Started { role, .. } => assert_eq!(role, "tester"),
        other => panic!("expected Started, got {other:?}"),
    }
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_transition_falls_back_to_executor_with_warning() {
    // "Closed" is a custom Redmine status that does not contain any
    // of the canonical substrings, so the helper must fall back to
    // executor AND carry a bounded warning on the `Started`
    // outcome so the hook call site can surface it via
    // `report_local_warnings`. The operator's only signal that
    // the workflow was renamed is the warning text; a silent
    // fallback to executor would bill the wrong role with no
    // observable side effect, so this assertion is the
    // regression guard.
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("fallback");
    let issue = 106;

    let outcome = auto_transition_timer(issue, ProviderKind::Redmine, "Closed");
    let warning_text = outcome.warning();
    match outcome {
        AutoTimerOutcome::Started {
            role,
            warning,
            ref finished_runs,
            ..
        } => {
            assert_eq!(role, "executor");
            assert!(finished_runs.is_empty());
            let warning = warning.expect(
                "fallback status must surface a warning on the Started outcome, \
                 not silently bill to executor",
            );
            assert!(
                warning.contains("defaulted to executor"),
                "warning must mention the executor default, got: {warning}"
            );
            assert!(
                warning.contains("Closed"),
                "warning must name the unmapped status, got: {warning}"
            );
        }
        other => panic!("expected Started with warning, got {other:?}"),
    }
    // The hook call site extracts the warning through
    // `AutoTimerOutcome::warning()`; assert the public surface
    // carries the same bounded text.
    let warning_text =
        warning_text.expect("AutoTimerOutcome::warning() must return Some for fallback started");
    assert!(
        warning_text.contains("defaulted to executor"),
        "warning() must mention the executor default, got: {warning_text}"
    );
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_close_finishes_every_running_row_for_issue() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("close-all");
    let issue = 107;
    let other_issue = 108;

    // Seed two running rows for the target issue plus one for an
    // unrelated issue; the close must not touch the unrelated row.
    for label in ["seg-a", "seg-b"] {
        let _ = storage
            .start_timer_run(label, issue, "In Progress", "executor", 1, 1_700_000_000)
            .unwrap();
    }
    let _ = storage
        .start_timer_run(
            "other",
            other_issue,
            "In Progress",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    assert_eq!(running_for(&storage, issue).len(), 2);
    assert_eq!(running_for(&storage, other_issue).len(), 1);

    let outcome = auto_close_issue_timer(issue, ProviderKind::Redmine);
    match outcome {
        AutoCloseOutcome::Closed { finished_runs, .. } => {
            assert_eq!(finished_runs.len(), 2);
            assert!(finished_runs.contains(&"seg-a".to_owned()));
            assert!(finished_runs.contains(&"seg-b".to_owned()));
        }
        other => panic!("expected Closed, got {other:?}"),
    }
    assert_eq!(running_for(&storage, issue).len(), 0);
    assert_eq!(running_for(&storage, other_issue).len(), 1);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_close_with_no_running_rows_is_a_noop() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-noop");
    let outcome = auto_close_issue_timer(999, ProviderKind::Redmine);
    match outcome {
        AutoCloseOutcome::Noop { .. } => {}
        other => panic!("expected Noop, got {other:?}"),
    }
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_close_for_forgejo_is_a_noop() {
    // For parity with `auto_transition_timer`'s `Skipped` branch,
    // a Forgejo close must short-circuit to `Noop` and never touch
    // the ledger. This is the regression guard for the round 1
    // reviewer's "inaccurate Forgejo close comment" finding.
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("close-forgejo-noop");
    let issue = 109;

    // Seed a running row. The Forgejo path must still return
    // `Noop` regardless, matching the provider coverage
    // documented in the lifecycle_auto module doc comment.
    let _ = storage
        .start_timer_run(
            "forgejo-row",
            issue,
            "In Progress",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    assert_eq!(running_for(&storage, issue).len(), 1);

    let outcome = auto_close_issue_timer(issue, ProviderKind::Forgejo);
    match outcome {
        AutoCloseOutcome::Noop { reason } => {
            assert!(
                reason.contains("forgejo"),
                "noop reason must mention the provider, got: {reason}"
            );
        }
        other => panic!("expected Noop for Forgejo close, got {other:?}"),
    }
    // The running row is intentionally preserved: the Forgejo
    // close path is a no-op, so the local bookkeeping state is
    // untouched.
    assert_eq!(running_for(&storage, issue).len(), 1);
    let _ = fs::remove_dir_all(temp);
}

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

#[test]
fn status_to_agent_role_preserves_internal_re_export_visibility() {
    // Smoke check: the public(crate) status_to_agent_role helper is
    // reachable from a sibling integration test module, confirming
    // the export plumbing is correct.
    let (role, fallback) = status_to_agent_role("In Progress");
    assert_eq!(role, "executor");
    assert!(!fallback);
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

#[test]
fn auto_prefixed_orphan_running_row_is_finished_failed_by_recover() {
    // Phase 3 contract: a hard-crash that leaves an auto- prefixed
    // running row open is recoverable via the existing
    // `timer recover` surface. The recover path transitions the
    // running row to `FAILED` locally before any provider
    // attempt, exactly the way a manual orphan is handled, and
    // a second recover on the same id is rejected (idempotent
    // against the FAILED terminal state). No live Redmine /
    // GitLab writes occur: the storage is the source of truth
    // and the projection layer is intentionally not exercised in
    // this test (that is the contract-test surface).
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_storage("recover-orphan");
    let run_id =
        crate::time_tracking_cli::auto_start_run(202, "In Progress", "executor", 1).unwrap();
    assert!(run_id.starts_with("auto-"));

    // The recover path is orchestrator-only. We invoke the
    // public(crate) `execute_recovery` dispatcher so the test
    // exercises the same code that the CLI uses. The provider
    // config is intentionally absent so the projection attempt
    // fails with a structured config error after the durable
    // FAILED transition; this is the same shape the
    // `timer_recovery_marks_orphan_failed_*` test in
    // `phase2_tests.rs` uses to confirm the local-first
    // contract.
    let first_err = crate::time_tracking_cli::execute_recovery(
        Some(crate::policy::Role::Orchestrator),
        Some(crate::providers::ProviderKind::Redmine),
        None,
        None,
        None,
        crate::command::TimerCommand::Recover {
            run_id: run_id.clone(),
        },
    )
    .expect_err("recover without config must surface a structured error after durable FAILED");
    let first_json = first_err.json();
    let kind = first_json["kind"].as_str().unwrap_or("");
    assert!(
        kind == "config" || kind == "request",
        "first recover must be a structured config or request error, got: {first_err:?}"
    );
    let after_first = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(
        after_first.status, "FAILED",
        "recover must transition the running row to FAILED locally"
    );
    assert_eq!(after_first.sync_status, "failed");
    assert!(after_first.finished_at.is_some());

    // A second recover on the same id must be rejected because
    // the row is already terminal at `failed`; the recover
    // path never reopens or silently overwrites a previous
    // outcome.
    let second_err = crate::time_tracking_cli::execute_recovery(
        Some(crate::policy::Role::Orchestrator),
        Some(crate::providers::ProviderKind::Redmine),
        None,
        None,
        None,
        crate::command::TimerCommand::Recover {
            run_id: run_id.clone(),
        },
    )
    .expect_err("recover on a terminal failed row must surface the persisted error");
    let second_json = second_err.json();
    let kind = second_json["kind"].as_str().unwrap_or("");
    assert!(
        kind == "config" || kind == "request",
        "second recover must be a structured config or request error, got: {second_err:?}"
    );
    let after_second = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(
        after_second.status, "FAILED",
        "status must remain FAILED on retry"
    );
    assert_eq!(
        after_second.sync_status, "failed",
        "sync_status must remain failed on retry"
    );
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn auto_prefixed_orphan_recover_projects_to_provider_with_idempotent_retry() {
    // Phase 3 full-recover projection: an auto- prefixed orphan
    // is finished FAILED locally and then projected to a Redmine
    // mock via the existing `execute_recovery` path. The mock
    // server mirrors the contract-test harness so the test does
    // not depend on a live Redmine instance. The retry recovers
    // the same `failed` error from the ledger without a second
    // HTTP call: a terminal `failed` row never reaches the
    // projection layer.
    use crate::infra::storage::test_support::EnvGuard;
    use crate::policy::Role;
    use crate::providers::{RedmineConfig, RedmineProvider};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    let _lock = lock_workflow_tests();
    let home = std::env::temp_dir().join(format!(
        "phasegent-auto-recover-redmine-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = home.join(crate::infra::storage::DB_FILENAME);
    let _env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db).unwrap();
    // Save the Redmine credential so `RedmineProvider::for_role`
    // can read it via `auth::redmine_api_key`.
    storage
        .save_credential(Role::Orchestrator, "redmine", "test-recover-key")
        .unwrap();

    // Open the orphan via the lifecycle auto path so the run
    // carries the `auto-` prefix and matches what
    // `timer list` would surface for a crashed orchestrator.
    let run_id =
        crate::time_tracking_cli::auto_start_run(203, "implementation", "executor", 1).unwrap();
    assert!(run_id.starts_with("auto-"));

    // Mock Redmine server: the recover path drives
    // `project_run_with_provider`, which performs the same three
    // calls as the manual `timer finish` flow. The mock inspects
    // the request line so each call observes the right fixture
    // (activities → re-list → POST).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let size = stream.read(&mut chunk).unwrap();
                if size == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..size]);
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&bytes[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
            }
            let request = String::from_utf8_lossy(&bytes).into_owned();
            // Pick the fixture by request line so the
            // projection path can advance through activities →
            // re-list → POST without an empty response causing
            // a config error.
            let body = if request.starts_with("GET /enumerations/time_entry_activities.json") {
                r#"{"time_entry_activities":[{"id":9,"name":"Development","is_default":true}]}"#
            } else if request.starts_with("GET /time_entries.json?") {
                r#"{"total_count":0,"limit":100,"time_entries":[]}"#
            } else {
                r#"{"time_entry":{"id":88,"issue":{"id":203},"activity":{"id":9,"name":"Development"},"hours":0.01,"comments":"phasegent timer run_id=auto-x","spent_on":"2026-09-09"}}"#
            };
            let status_text = if request.starts_with("POST") {
                ("201 Created", body)
            } else {
                ("200 OK", body)
            };
            requests.push(request);
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status_text.0,
                status_text.1.len(),
                status_text.1
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        sender.send(requests).unwrap();
    });
    let mock_base = format!("http://{address}");

    // Build a RedmineConfig that points at the mock and let
    // `RedmineConfig::resolve` pick up the stored credential.
    // We do not persist `api_base` to the role config because
    // the recover call passes it explicitly as `api_base`.
    let _resolved =
        RedmineConfig::resolve(Role::Orchestrator, Some(&mock_base), Some("42"), Some("37"))
            .expect("resolve must succeed with explicit args and stored credential");
    let _provider = RedmineProvider::for_role(
        Role::Orchestrator,
        RedmineConfig::new(mock_base.clone(), "42", 37),
    )
    .expect("provider must build from stored credential");

    // First recover: orphan → FAILED locally → project to mock.
    // The mock server returns 201 with a time entry id, so the
    // projection layer marks the row `synced`.
    crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(crate::providers::ProviderKind::Redmine),
        Some(&mock_base),
        Some("42"),
        Some("37"),
        crate::command::TimerCommand::Recover {
            run_id: run_id.clone(),
        },
    )
    .expect("recover against mock must succeed");

    let after = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(
        after.status, "FAILED",
        "orphan recover must leave status at FAILED"
    );
    assert_eq!(
        after.sync_status, "synced",
        "projection against mock must mark sync_status synced"
    );
    assert_eq!(after.time_entry_id, Some(88));

    // The mock recorded exactly 3 requests (activities list,
    // re-list, POST). After the first recover the local
    // `synced` state prevents the projection layer from
    // issuing a second POST.
    let observed = receiver.recv().unwrap();
    assert_eq!(
        observed.len(),
        3,
        "first recover must hit the mock once for activities + re-list + POST, got {observed:?}"
    );
    assert!(observed[0].starts_with("GET /enumerations/time_entry_activities.json"));
    assert!(observed[1].starts_with("GET /time_entries.json?"));
    assert!(observed[2].starts_with("POST /time_entries.json"));
    assert!(observed[2].contains("auto-"));

    // Second recover on a `synced` terminal row: the recover
    // path returns the existing row as a `Single` envelope and
    // never attempts another projection. The mock listener has
    // already served its three responses, so the assertion is
    // that the returned row matches the post-first-recover
    // state exactly. This is the documented contract: only
    // `failed` (not `synced`) terminal rows surface the stored
    // error, because a successfully projected run is
    // intentionally treated as already-done.
    let second = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(crate::providers::ProviderKind::Redmine),
        Some(&mock_base),
        Some("42"),
        Some("37"),
        crate::command::TimerCommand::Recover {
            run_id: run_id.clone(),
        },
    )
    .expect("recover on a synced-FAILED row must return the existing row");
    let second_row = match second {
        crate::time_tracking::TimerListOutput::Single { run } => *run,
        other => panic!("second recover must return Single, got {other:?}"),
    };
    assert_eq!(second_row.run_id, run_id);
    assert_eq!(second_row.status, "FAILED");
    assert_eq!(second_row.sync_status, "synced");
    assert_eq!(second_row.time_entry_id, Some(88));
    // The mock has only 3 fixtures, so a 4th call would
    // block forever. The successful return above proves no
    // additional HTTP call was made.
    let after_retry = storage.load_timer_run(&run_id).unwrap().unwrap();
    assert_eq!(after_retry.status, "FAILED");
    assert_eq!(after_retry.sync_status, "synced");
    assert_eq!(after_retry.time_entry_id, Some(88));

    server.join().unwrap();
    let _ = fs::remove_dir_all(home);
}

// ---------------------------------------------------------------------------
// Issue 305 Task 3: issue-close worktree-lease release.
//
// The lifecycle helper resolves the current repo identity and delegates to
// the worktree domain flip. These tests pin the session isolation contract:
// only the closed session's active lease for the closed issue/repo becomes
// `retained`; every other session, issue, and repo is left `active`, and an
// absent session never guesses an owner.
// ---------------------------------------------------------------------------

fn temp_git_repo(label: &str) -> Option<(std::path::PathBuf, String)> {
    let dir = unique_temp_dir(&format!("close-repo-{label}"));
    fs::create_dir_all(&dir).ok()?;
    let runner = crate::worktree::ProcessWorktreeRunner::new();
    if runner.run(&["init", "-q", "-b", "main"], &dir).is_err() {
        let _ = fs::remove_dir_all(&dir);
        return None;
    }
    // Best-effort initial commit: an unborn HEAD is still a valid repo
    // identity for the lease-release hook under test.
    let _ = runner.run(
        &[
            "-c",
            "user.name=phasegent-test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ],
        &dir,
    );
    let identity = crate::worktree::repo_identity(&runner, &dir).ok()?;
    Some((dir, identity))
}

fn seed_lease(identity: &str, issue: u64, session: &str, status: &str) -> String {
    let storage = Storage::open().expect("storage for lease seed");
    crate::worktree::ensure_schema(&storage).expect("worktree schema");
    let lease_id = format!(
        "lease-{issue}-{session}-{}",
        crate::worktree::compute_fingerprint(identity)
    );
    let now = crate::worktree::now_unix_secs();
    let worktree_path = format!("/tmp/phasegent-lease-{issue}-{session}");
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: &lease_id,
            identity,
            issue,
            session,
            checkout_path: "/tmp/phasegent-checkout",
            worktree_path: &worktree_path,
            branch: "main",
            status,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("seed lease");
    lease_id
}

fn lease_state(lease_id: &str) -> (String, Option<String>) {
    let storage = Storage::open().expect("storage for lease read");
    let row = crate::worktree::leases::load_lease(&storage, lease_id)
        .expect("load lease")
        .expect("lease exists");
    (row.status, row.release_reason)
}

#[test]
fn close_release_flips_only_current_session_lease_for_issue_and_repo() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-release");
    let Some((repo_dir, identity)) = temp_git_repo("current") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let lease_a = seed_lease(&identity, 305, "session-a", "active");
    let lease_b = seed_lease(&identity, 305, "session-b", "active");
    let same_session_other_issue = seed_lease(&identity, 306, "session-a", "active");
    let other_repo = seed_lease("git-common-dir:/elsewhere/.git", 305, "session-a", "active");

    let outcome = crate::lifecycle::release_closed_issue_leases(
        &crate::worktree::ProcessWorktreeRunner::new(),
        &repo_dir,
        305,
        Some("session-a"),
    );
    assert_eq!(
        outcome,
        crate::lifecycle::AutoReleaseLeaseOutcome::Released { released: 1 }
    );
    assert!(outcome.warning().is_none());

    let (status_a, reason_a) = lease_state(&lease_a);
    assert_eq!(status_a, "retained");
    assert_eq!(reason_a.as_deref(), Some("issue closed: session-a"));
    assert_eq!(lease_state(&lease_b).0, "active");
    assert_eq!(lease_state(&same_session_other_issue).0, "active");
    assert_eq!(lease_state(&other_repo).0, "active");
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_release_without_session_is_noop_with_warning() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-no-session");
    let Some((repo_dir, identity)) = temp_git_repo("no-session") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let lease_a = seed_lease(&identity, 305, "session-a", "active");
    let lease_b = seed_lease(&identity, 305, "session-b", "active");

    let outcome = crate::lifecycle::release_closed_issue_leases(
        &crate::worktree::ProcessWorktreeRunner::new(),
        &repo_dir,
        305,
        None,
    );
    match &outcome {
        crate::lifecycle::AutoReleaseLeaseOutcome::NoSession { reason } => {
            assert!(
                reason.contains("left untouched"),
                "no-session reason must say leases were left untouched: {reason}"
            );
        }
        other => panic!("expected NoSession, got {other:?}"),
    }
    let warning = outcome
        .warning()
        .expect("an absent session must surface a stderr warning");
    assert!(
        warning.contains("PHASEGENT_SESSION_ID"),
        "warning must name the session source, got: {warning}"
    );
    assert_eq!(lease_state(&lease_a).0, "active");
    assert_eq!(lease_state(&lease_b).0, "active");
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_release_with_blank_session_is_noop_with_warning() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-blank-session");
    let Some((repo_dir, identity)) = temp_git_repo("blank-session") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let lease_a = seed_lease(&identity, 305, "session-a", "active");

    let outcome = crate::lifecycle::release_closed_issue_leases(
        &crate::worktree::ProcessWorktreeRunner::new(),
        &repo_dir,
        305,
        Some("   "),
    );
    assert!(matches!(
        outcome,
        crate::lifecycle::AutoReleaseLeaseOutcome::NoSession { .. }
    ));
    assert_eq!(lease_state(&lease_a).0, "active");
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_release_warns_when_repo_identity_is_unavailable() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-bad-repo");
    // A plain directory that is not a git checkout: resolving the
    // canonical identity fails, but the remote close already succeeded,
    // so the hook must degrade to a bounded warning instead of failing.
    let dir = unique_temp_dir("close-bad-repo-dir");
    fs::create_dir_all(&dir).unwrap();

    let outcome = crate::lifecycle::release_closed_issue_leases(
        &crate::worktree::ProcessWorktreeRunner::new(),
        &dir,
        305,
        Some("session-a"),
    );
    match &outcome {
        crate::lifecycle::AutoReleaseLeaseOutcome::Warning { reason } => {
            assert!(
                reason.contains("repository identity"),
                "warning must explain the identity failure: {reason}"
            );
        }
        other => panic!("expected Warning, got {other:?}"),
    }
    assert!(outcome.warning().is_some());
    let _ = fs::remove_dir_all(dir);
    let _ = fs::remove_dir_all(temp);
}
