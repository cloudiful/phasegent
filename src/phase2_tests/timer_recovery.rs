use super::*;

#[test]
fn timer_recovery_marks_orphan_failed_and_is_idempotent_for_terminal_rows() {
    use crate::infra::storage::test_support::lock_workflow_tests;
    use crate::infra::storage::{Storage, TIMER_SYNC_FAILED};
    let _lock = lock_workflow_tests();
    let home = crate::test_scratch::root().join(format!(
        "phasegent-timer-recover-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = home.join(crate::infra::storage::DB_FILENAME);
    let _env = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db_path.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .start_timer_run(
            "recover-run",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();

    // Without provider credentials the recover path must durably record
    // FAILED locally and then surface the projection failure as a
    // structured nonzero error, never as a successful envelope.
    let first_err = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Recover {
            run_id: "recover-run".to_owned(),
        },
    )
    .unwrap_err();
    // Structured error (config/request) after durable FAILED.
    assert!(
        first_err.json()["kind"] == "config" || first_err.json()["kind"] == "request",
        "first recover must be structured error, got {:?}",
        first_err.json()
    );
    let persisted = Storage::open_at(&db_path)
        .unwrap()
        .load_timer_run("recover-run")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.status, "FAILED");
    assert_eq!(persisted.sync_status, TIMER_SYNC_FAILED);
    assert!(persisted.sync_error.is_some());
    let first_sync_error = persisted.sync_error.clone();

    // A second recover on the same id must not claim success while the
    // projection is still failed; it surfaces the same failure rather
    // than returning a successful envelope with sync_failed.
    let second_err = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Recover {
            run_id: "recover-run".to_owned(),
        },
    )
    .unwrap_err();
    assert!(
        second_err.json()["kind"] == "config" || second_err.json()["kind"] == "request",
        "second recover must also be error, got {:?}",
        second_err.json()
    );
    let second_row = Storage::open_at(&db_path)
        .unwrap()
        .load_timer_run("recover-run")
        .unwrap()
        .unwrap();
    assert_eq!(second_row.status, "FAILED");
    assert_eq!(second_row.sync_status, TIMER_SYNC_FAILED);
    assert_eq!(second_row.sync_error, first_sync_error);

    // Unknown run ids return a structured config error and never touch
    // the network.
    let missing = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Get {
            run_id: "no-such-run".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(missing.json()["kind"], "config");
    assert!(
        missing.json()["message"]
            .as_str()
            .unwrap_or("")
            .contains("was not found")
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_recover_with_explicit_forgejo_marks_failed_and_returns_not_supported() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    use crate::infra::storage::{Storage, TIMER_SYNC_FAILED};
    let _lock = lock_workflow_tests();
    let home = crate::test_scratch::root().join(format!(
        "phasegent-timer-recover-forgejo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = home.join(crate::infra::storage::DB_FILENAME);
    let _env = EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db_path.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .start_timer_run(
            "recover-forgejo",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();
    let err = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Forgejo),
        None,
        None,
        None,
        command::TimerCommand::Recover {
            run_id: "recover-forgejo".to_owned(),
        },
    )
    .unwrap_err();
    assert_eq!(err.json()["kind"], "not_supported");
    let row = Storage::open_at(&db_path)
        .unwrap()
        .load_timer_run("recover-forgejo")
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "FAILED");
    assert_eq!(row.sync_status, TIMER_SYNC_FAILED);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn timer_list_and_get_return_local_only_payloads_without_network() {
    use crate::infra::storage::Storage;
    use crate::infra::storage::test_support::lock_workflow_tests;
    let _lock = lock_workflow_tests();
    let home = crate::test_scratch::root().join(format!(
        "phasegent-timer-listget-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_path = home.join(crate::infra::storage::DB_FILENAME);
    let _env = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_DB_PATH",
        db_path.as_os_str().to_string_lossy().as_ref(),
    );
    let storage = Storage::open_at(&db_path).unwrap();
    storage
        .start_timer_run("lg-1", 28, "implementation", "executor", 1, 1_700_000_000)
        .unwrap();
    storage
        .finish_timer_run("lg-1", "DONE", 1_700_000_037)
        .unwrap();

    let list = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::List {
            status: "all".to_owned(),
            limit: 100,
        },
    )
    .unwrap();
    match list {
        crate::time_tracking_cli::TimerListOutput::Many { runs, count } => {
            assert_eq!(count, 1);
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].run_id, "lg-1");
        }
        other => panic!("expected many envelope, got {other:?}"),
    }

    let get = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Redmine),
        None,
        None,
        None,
        command::TimerCommand::Get {
            run_id: "lg-1".to_owned(),
        },
    )
    .unwrap();
    match get {
        crate::time_tracking_cli::TimerListOutput::Single { run } => {
            let run = *run;
            assert_eq!(run.run_id, "lg-1");
            assert_eq!(run.status, "DONE");
            assert_eq!(run.elapsed_seconds, Some(37));
        }
        other => panic!("expected single envelope, got {other:?}"),
    }

    // The same calls against Forgejo remain available so an operator
    // listing or inspecting an orphan does not require a provider
    // switch; recover is still Redmine/GitLab-only.
    let list_forgejo = crate::time_tracking_cli::execute_recovery(
        Some(Role::Orchestrator),
        Some(ProviderKind::Forgejo),
        None,
        None,
        None,
        command::TimerCommand::List {
            status: "running".to_owned(),
            limit: 100,
        },
    )
    .unwrap();
    match list_forgejo {
        crate::time_tracking_cli::TimerListOutput::Many { runs, .. } => {
            // Only one row exists and it is finished, so the running
            // filter must surface an empty list.
            assert!(runs.is_empty());
        }
        other => panic!("expected many envelope, got {other:?}"),
    }
    let _ = fs::remove_dir_all(home);
}
