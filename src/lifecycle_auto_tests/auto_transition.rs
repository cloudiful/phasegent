use super::support::*;
use super::*;

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
