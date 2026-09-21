use super::support::*;
use super::*;

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
