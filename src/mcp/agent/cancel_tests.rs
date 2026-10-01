//! Cancellation and run-registration regression tests.
//!
//! A checkpoint review found a cancel that could not reach the agent
//! (the prompt held the connection lock for its whole turn) and a run
//! whose in-memory handle was published only after its task could
//! already finish. These pin both boundaries, plus the settlement
//! property the second repair added: a cancel must leave a terminal row
//! even when it finds no live handle to pull on.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::duplex;

use super::run::{CANCEL_GRACE, RunManager};
use super::session::AcpSession;
use super::test_kit::{FakeAgent, FakeAgentOptions};
use super::types::{AcpSpawnConfig, ResearchPrompt};

use super::tests::{temp_db_path, wait_terminal};

/// A live session whose turn only ends when the agent is cancelled.
async fn hanging_session() -> (Arc<AcpSession>, Arc<FakeAgent>) {
    let agent = Arc::new(FakeAgent::new(FakeAgentOptions {
        prompt_awaits_cancel: true,
        ..FakeAgentOptions::default()
    }));
    let (client_side, agent_side) = duplex(64 * 1024);
    let server = agent.clone();
    tokio::spawn(async move { server.serve(agent_side).await });
    let session = Arc::new(
        AcpSession::start_in_process(client_side, "/tmp/fake-worktree".to_owned())
            .await
            .expect("in-process session"),
    );
    session.negotiate_research().await.expect("negotiate");
    (session, agent)
}

#[tokio::test]
async fn cancel_reaches_the_agent_while_a_prompt_is_in_flight() {
    let (session, _agent) = hanging_session().await;
    let prompt = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .prompt(&ResearchPrompt::new("long recon").with_timeout_secs(30))
                .await
        }
    });
    // Give the prompt time to occupy the connection, then cancel. If
    // cancel waited on the prompt's own request slot this would block
    // for the full 30s timeout instead of returning promptly.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = std::time::Instant::now();
    session
        .cancel()
        .await
        .expect("cancel reaches the agent during a prompt");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancel must not serialize behind the in-flight turn"
    );
    let outcome = tokio::time::timeout(Duration::from_secs(10), prompt)
        .await
        .expect("the cancelled turn resolves")
        .expect("prompt task")
        .expect("cancelled turn still reports its stop reason");
    assert_eq!(outcome.stop_reason, super::types::StopReason::Cancelled);
    session.kill().await;
}

#[tokio::test]
async fn kill_is_idempotent_and_unblocks_an_in_flight_turn() {
    let (session, _agent) = hanging_session().await;
    let prompt = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .prompt(&ResearchPrompt::new("long recon").with_timeout_secs(30))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    session.kill().await;
    let error = tokio::time::timeout(Duration::from_secs(10), prompt)
        .await
        .expect("killing the process unblocks the turn")
        .expect("prompt task")
        .expect_err("a killed process has no answer");
    assert_eq!(error.kind.as_str(), "closed");
    // A second teardown is a no-op rather than a panic.
    session.kill().await;
}

#[tokio::test]
async fn run_registration_precedes_the_task_so_a_fast_run_is_still_visible() {
    // The spawn-failure task finishes almost immediately. If the
    // in-memory handle were published after the spawn, a worker polling
    // the task first would remove a registration that did not exist yet
    // and the run id would stay permanently unusable.
    let path = temp_db_path("registration");
    let manager = RunManager::new(super::tests::open_storage(&path)).expect("manager");
    let config = AcpSpawnConfig {
        program: "phasegent-definitely-not-a-binary".to_owned(),
        cwd: std::env::temp_dir(),
        handshake_timeout_secs: None,
    };
    for attempt in 0..25 {
        let run_id = format!("race-{attempt}");
        manager
            .start_run(
                &run_id,
                "ses_cancel",
                config.clone(),
                ResearchPrompt::new("go"),
            )
            .await
            .expect("run accepted");
        for _ in 0..200 {
            if !manager.is_in_flight(&run_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !manager.is_in_flight(&run_id),
            "run {run_id} must leave the registry"
        );
        let reader = super::tests::open_storage(&path);
        let run = reader.load_work_run(&run_id).unwrap().expect("row");
        assert_eq!(
            run.status, "failed",
            "run {run_id} reached a terminal state"
        );
    }
}

#[tokio::test]
async fn a_registered_run_id_cannot_be_started_twice() {
    let path = temp_db_path("duplicate");
    let manager = RunManager::new(super::tests::open_storage(&path)).expect("manager");
    let config = AcpSpawnConfig {
        program: "phasegent-definitely-not-a-binary".to_owned(),
        cwd: std::env::temp_dir(),
        handshake_timeout_secs: None,
    };
    manager
        .start_run(
            "dup-run",
            "ses_dup",
            config.clone(),
            ResearchPrompt::new("go"),
        )
        .await
        .expect("first run accepted");
    let error = manager
        .start_run("dup-run", "ses_dup", config, ResearchPrompt::new("go"))
        .await
        .expect_err("an active run id must not be reused");
    assert!(
        error.message.contains("already active"),
        "{}",
        error.message
    );
    let reader = super::tests::open_storage(&path);
    wait_terminal(&reader, "dup-run").await;
}

#[tokio::test]
async fn cancelling_a_spawning_run_records_a_terminal_cancelled_status() {
    // A run whose process can never spawn is cancelled while it is
    // still launching. Either the launch task observes the intent and
    // writes `cancelled`, or the abort escalation does; the row must
    // never be left `pending` or `running`.
    let path = temp_db_path("cancel-launching");
    let storage = super::tests::open_storage(&path);
    let manager = RunManager::new(storage).expect("manager");
    let config = AcpSpawnConfig {
        program: "phasegent-definitely-not-a-binary".to_owned(),
        cwd: std::env::temp_dir(),
        handshake_timeout_secs: None,
    };
    manager
        .start_run(
            "launch-cancel",
            "ses_launch",
            config,
            ResearchPrompt::new("go"),
        )
        .await
        .expect("run accepted");
    let row = manager
        .cancel_run("launch-cancel")
        .await
        .expect("cancel accepted");
    assert!(
        super::store::TERMINAL_STATUSES.contains(&row.status.as_str()),
        "a cancelled run must be terminal, got {}",
        row.status
    );
    let reader = super::tests::open_storage(&path);
    let final_row = wait_terminal(&reader, "launch-cancel").await;
    assert_eq!(final_row.status, "cancelled");
    assert!(!manager.is_in_flight("launch-cancel"));
    // Cancelling a settled run is idempotent, not an error.
    let again = manager
        .cancel_run("launch-cancel")
        .await
        .expect("second cancel is a no-op");
    assert_eq!(again.status, "cancelled");
}

#[tokio::test]
async fn cancelling_a_run_with_no_live_handle_still_records_the_cancel() {
    // A row can be left `running` with nothing registered behind it: a
    // process that died with the run task, or a caller that reaches the
    // ledger before recovery runs. Nothing else will ever settle it, so
    // the cancel itself has to, or the run stays open forever.
    let path = temp_db_path("cancel-unregistered");
    let manager = RunManager::new(super::tests::open_storage(&path)).expect("manager");
    let writer = super::tests::open_storage(&path);
    writer
        .create_work_run("orphaned", "/tmp/wt", "go")
        .expect("create");
    writer
        .update_work_run(
            "orphaned",
            &super::store::WorkRunUpdate {
                status: Some("running".to_owned()),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .expect("mark running");
    assert!(!manager.is_in_flight("orphaned"));

    let row = manager.cancel_run("orphaned").await.expect("cancel");
    assert_eq!(row.status, "cancelled", "the cancel is durable");
    assert!(row.error.is_some(), "the reason is recorded");
    assert!(row.finished_at.is_some());
    let again = manager.cancel_run("orphaned").await.expect("idempotent");
    assert_eq!(again.status, "cancelled", "a terminal row is not reopened");
}

#[tokio::test]
async fn cancel_grace_stays_bounded_when_the_agent_never_acknowledges() {
    // A `sleep` binary answers nothing on stdin, so the handshake times
    // out on its own; the cancel must still return within its budget
    // rather than waiting for the run's own timeout.
    let path = temp_db_path("cancel-bounded");
    let storage = super::tests::open_storage(&path);
    let manager = RunManager::new(storage).expect("manager");
    let config = AcpSpawnConfig {
        program: "sleep".to_owned(),
        cwd: std::env::temp_dir(),
        handshake_timeout_secs: None,
    };
    manager
        .start_run(
            "bounded",
            "ses_bounded",
            config,
            ResearchPrompt::new("go").with_timeout_secs(3),
        )
        .await
        .expect("run accepted");
    let started = std::time::Instant::now();
    let row = manager
        .cancel_run("bounded")
        .await
        .expect("cancel accepted");
    assert!(
        started.elapsed() < CANCEL_GRACE * 3,
        "cancel must be bounded, took {:?}",
        started.elapsed()
    );
    assert!(
        super::store::TERMINAL_STATUSES.contains(&row.status.as_str()),
        "cancelled row is terminal, got {}",
        row.status
    );
}
