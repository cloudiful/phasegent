//! Ownership and bounded-wait tests for the research run manager (issue 685
//! P2).
//!
//! The manager is a library, so these drive the ledger directly instead of
//! spawning a real ACP process: the properties under test are who may act on a
//! run, and whether a bounded wait returns instead of hanging. The ACP
//! lifecycle itself is covered by `process_tests`, `cancel_tests`, and
//! `resume_tests`.

use std::time::Duration;

use crate::infra::storage::Storage;
use crate::mcp::agent::run::shared;
use crate::mcp::agent::store::{
    RUN_COMPLETED, RUN_INTERRUPTED, RUN_PENDING, RUN_RUNNING, WorkRunUpdate, now_epoch_seconds,
};
use crate::mcp::agent::store_tests::open_storage;
use crate::mcp::agent::{AcpSpawnConfig, ResearchPrompt, RunManager};

const OWNER: &str = "ses_owner";
const OTHER: &str = "ses_other";

/// A manager over its own scratch ledger, with one run already owned by
/// [`OWNER`] and left in the requested status.
fn manager_with(label: &str, run_id: &str, status: &str) -> RunManager {
    let storage = open_storage(label);
    let manager = RunManager::new(storage).expect("manager");
    manager
        .connection()
        .create_work_run(run_id, "/tmp/wt-a", "recon the call flow")
        .expect("seed run");
    manager
        .connection()
        .bind_run_owner(run_id, OWNER)
        .expect("seed owner");
    if status != RUN_PENDING {
        manager
            .connection()
            .update_work_run(
                run_id,
                &WorkRunUpdate {
                    status: Some(status.to_owned()),
                    finished_at: Some(now_epoch_seconds()),
                    ..WorkRunUpdate::default()
                },
            )
            .expect("seed status");
    }
    manager
}

#[test]
fn an_owned_run_is_visible_only_to_its_owner() {
    let manager = manager_with("owner-visible", "run-1", RUN_RUNNING);
    assert!(manager.owned_run("run-1", OWNER).is_ok());
    let error = manager
        .owned_run("run-1", OTHER)
        .expect_err("another session must not see the run");
    assert!(error.contains("not available"), "{error}");
    assert!(manager.owned_run("absent", OWNER).is_err());
    // The refusal names neither the owner nor the scratch path.
    assert!(!error.contains(OWNER), "{error}");
    assert!(!error.contains("/tmp/wt-a"), "{error}");
}

#[test]
fn a_run_without_an_owner_row_is_unusable_by_every_session() {
    let storage = open_storage("owner-missing");
    let manager = RunManager::new(storage).expect("manager");
    manager
        .connection()
        .create_work_run("run-1", "/tmp/wt-a", "recon")
        .expect("seed run");
    assert!(manager.owned_run("run-1", OWNER).is_err());
    assert!(manager.owned_run("run-1", OTHER).is_err());
}

#[tokio::test]
async fn a_bounded_wait_returns_the_row_instead_of_hanging() {
    let manager = manager_with("wait-bounded", "run-1", RUN_RUNNING);
    let started = std::time::Instant::now();
    let row = manager
        .wait_run("run-1", OWNER, Duration::from_millis(120))
        .await
        .expect("bounded wait");
    assert_eq!(row.status, RUN_RUNNING);
    assert!(!RunManager::is_terminal(&row));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a bounded wait must return on its budget, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_bounded_wait_returns_as_soon_as_the_run_is_terminal() {
    let manager = manager_with("wait-terminal", "run-1", RUN_COMPLETED);
    let row = manager
        .wait_run("run-1", OWNER, Duration::from_secs(30))
        .await
        .expect("terminal wait");
    assert_eq!(row.status, RUN_COMPLETED);
    assert!(RunManager::is_terminal(&row));
}

#[tokio::test]
async fn a_foreign_session_can_neither_wait_nor_cancel_a_run() {
    let manager = manager_with("wait-foreign", "run-1", RUN_RUNNING);
    assert!(
        manager
            .wait_run("run-1", OTHER, Duration::from_millis(50))
            .await
            .is_err()
    );
    assert!(manager.cancel_owned_run("run-1", OTHER).await.is_err());
    // The refused cancel left the run untouched for its owner.
    assert_eq!(
        manager.owned_run("run-1", OWNER).expect("owner").status,
        RUN_RUNNING
    );
}

#[tokio::test]
async fn a_foreign_session_cannot_resume_another_sessions_interrupted_run() {
    let manager = manager_with("resume-foreign", "run-1", RUN_INTERRUPTED);
    let error = manager
        .resume_owned_run("run-1", OTHER, ResearchPrompt::new("continue"))
        .await
        .expect_err("another session must not resume the run");
    assert!(error.contains("not available"), "{error}");
    // The durable row is still terminal: a refused resume reopens nothing.
    assert_eq!(
        manager.owned_run("run-1", OWNER).expect("owner").status,
        RUN_INTERRUPTED
    );
    assert!(!manager.is_in_flight("run-1"));
}

/// A duplicate run id is refused for everyone, and a refused start never
/// transfers an existing run to the caller that attempted it.
#[tokio::test]
async fn a_start_with_an_existing_run_id_is_refused_and_transfers_nothing() {
    let manager = manager_with("start-foreign", "run-1", RUN_RUNNING);
    let error = manager
        .start_run(
            "run-1",
            OTHER,
            AcpSpawnConfig::new(std::path::PathBuf::from("/tmp/wt-a")),
            ResearchPrompt::new("recon"),
        )
        .await
        .expect_err("an existing run id is not reusable");
    assert!(
        error.message.contains("already exists"),
        "{}",
        error.message
    );
    assert!(
        manager.owned_run("run-1", OWNER).is_ok(),
        "the original owner keeps the run"
    );
    assert!(
        manager.owned_run("run-1", OTHER).is_err(),
        "the refused start must not transfer ownership"
    );
    assert!(!manager.is_in_flight("run-1"));
}

#[test]
fn one_database_resolves_to_one_shared_manager_across_handles() {
    let path = crate::mcp::agent::store_tests::temp_db_path("shared-manager");
    let first = shared(&Storage::open_at(&path).expect("first open")).expect("first manager");
    let repeat = shared(&Storage::open_at(&path).expect("second open")).expect("repeat manager");
    assert!(first.same_manager_as(&repeat));
}
