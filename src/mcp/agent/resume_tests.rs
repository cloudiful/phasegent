//! ACP session resume regression tests.
//!
//! A checkpoint review found the run ledger recorded an ACP session id
//! that nothing ever used. These cover the real resume path: a second
//! process reconnects through `session/load`, re-verifies its pinned
//! values from the load response, and continues the conversation.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::duplex;

use super::run::RunManager;
use super::session::AcpSession;
use super::test_kit::{FakeAgent, FakeAgentOptions};
use super::types::{ExplorerPrompt, StopReason};
use super::wire::{
    CONFIG_ID_MODEL, EXPLORER_MODEL_WIRE_VALUE, EXPLORER_PERMISSION_MODE, EXPLORER_THINKING_EFFORT,
};

use super::tests::{open_storage, temp_db_path, wait_terminal};

const WORKTREE: &str = "/tmp/fake-worktree";

fn agent_for(options: FakeAgentOptions) -> (tokio::io::DuplexStream, Arc<FakeAgent>) {
    let agent = Arc::new(FakeAgent::new(options));
    let (client_side, agent_side) = duplex(64 * 1024);
    let server = agent.clone();
    tokio::spawn(async move { server.serve(agent_side).await });
    (client_side, agent)
}

#[tokio::test]
async fn resume_reconnects_through_session_load_and_reverifies_the_values() {
    let (stream, agent) = agent_for(FakeAgentOptions::default());
    let session = AcpSession::resume_in_process(stream, WORKTREE.to_owned(), "fake-session-1")
        .await
        .expect("session/load resumes the persisted id");
    assert_eq!(session.session_id().await, "fake-session-1");
    assert_eq!(
        agent.loaded_sessions.lock().await.as_slice(),
        ["fake-session-1".to_owned()],
        "the persisted id is what the new process loads"
    );
    let report = session
        .negotiate_explorer()
        .await
        .expect("a resumed session re-selects and re-verifies");
    assert_eq!(report.model.as_deref(), Some(EXPLORER_MODEL_WIRE_VALUE));
    assert_eq!(report.effort.as_deref(), Some(EXPLORER_THINKING_EFFORT));
    assert_eq!(
        report.permission_mode.as_deref(),
        Some(EXPLORER_PERMISSION_MODE)
    );
    let selections = agent.selections.lock().await.clone();
    assert_eq!(selections.len(), 3, "every pinned value is re-applied");
    assert_eq!(selections[0].0, super::wire::CONFIG_ID_PERMISSION_MODE);
    assert_eq!(selections[1].0, CONFIG_ID_MODEL);
}

#[tokio::test]
async fn resume_requires_the_load_capability() {
    let (stream, _agent) = agent_for(FakeAgentOptions {
        no_load_session: true,
        ..FakeAgentOptions::default()
    });
    let error = AcpSession::resume_in_process(stream, WORKTREE.to_owned(), "fake-session-1")
        .await
        .expect_err("an agent without loadSession cannot be resumed");
    assert_eq!(error.kind.as_str(), "negotiation");
    assert!(error.message.contains("session/load"), "{}", error.message);
}

#[tokio::test]
async fn resume_fails_closed_on_a_refused_load_or_a_foreign_session() {
    let (stream, _agent) = agent_for(FakeAgentOptions {
        reject_load_session: true,
        ..FakeAgentOptions::default()
    });
    let error = AcpSession::resume_in_process(stream, WORKTREE.to_owned(), "fake-session-1")
        .await
        .expect_err("a refused load must not be treated as a resume");
    assert!(matches!(error.kind.as_str(), "protocol" | "negotiation"));

    let (stream, _agent) = agent_for(FakeAgentOptions {
        load_returns_other_session: true,
        ..FakeAgentOptions::default()
    });
    let error = AcpSession::resume_in_process(stream, WORKTREE.to_owned(), "fake-session-1")
        .await
        .expect_err("loading a different session is not a resume");
    assert_eq!(error.kind.as_str(), "negotiation");
    assert!(
        error.message.contains("different session id"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn resume_needs_a_persisted_session_id() {
    let (stream, _agent) = agent_for(FakeAgentOptions::default());
    let error = AcpSession::resume_in_process(stream, WORKTREE.to_owned(), "  ")
        .await
        .expect_err("a blank session id cannot be resumed");
    assert_eq!(error.kind.as_str(), "negotiation");
}

#[tokio::test]
async fn a_resumed_turn_streams_into_the_same_session() {
    let (stream, _agent) = agent_for(FakeAgentOptions::default());
    let session = AcpSession::resume_in_process(stream, WORKTREE.to_owned(), "fake-session-1")
        .await
        .expect("resume");
    session.negotiate_explorer().await.expect("negotiate");
    let outcome = session
        .prompt(&ExplorerPrompt::new("continue the recon"))
        .await
        .expect("the resumed session still runs turns");
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert_eq!(outcome.text, "found 12 modules in src/");
    session.kill().await;
}

#[tokio::test]
async fn resume_run_continues_an_interrupted_ledger_row() {
    let path = temp_db_path("resume-run");
    let storage = open_storage(&path);
    // A run whose process died: a `running` row with no in-process
    // handle, which recovery marks interrupted.
    storage
        .create_work_run("resumable", WORKTREE, "summarize src/")
        .expect("create");
    storage
        .update_work_run(
            "resumable",
            &super::store::WorkRunUpdate {
                status: Some("running".to_owned()),
                acp_session_id: Some("fake-session-1".to_owned()),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .expect("mark running");
    let manager = RunManager::new(storage).expect("manager");
    let row = manager.load("resumable").unwrap().expect("row");
    assert_eq!(row.status, "interrupted", "an orphaned run is recoverable");

    let reopened = manager
        .resume_run(
            "resumable",
            ExplorerPrompt::new("continue where you left off"),
        )
        .await
        .expect("resume accepted");
    assert_eq!(reopened.status, "pending");
    assert_eq!(reopened.acp_session_id.as_deref(), Some("fake-session-1"));
    // The durable row keeps its id and prompt; only the terminal
    // markers are cleared.
    assert_eq!(reopened.prompt, "summarize src/");
    assert!(reopened.finished_at.is_none());
    assert!(reopened.error.is_none());
    assert_eq!(reopened.worktree_cwd, WORKTREE);
}

#[tokio::test]
async fn resume_run_refuses_completed_and_sessionless_rows() {
    let path = temp_db_path("resume-refusals");
    let storage = open_storage(&path);
    storage
        .create_work_run("done", WORKTREE, "p")
        .expect("create");
    storage
        .update_work_run(
            "done",
            &super::store::WorkRunUpdate {
                status: Some("completed".to_owned()),
                acp_session_id: Some("ses-1".to_owned()),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .expect("complete");
    storage
        .create_work_run("no-session", WORKTREE, "p")
        .expect("create");
    storage
        .update_work_run(
            "no-session",
            &super::store::WorkRunUpdate {
                status: Some("interrupted".to_owned()),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .expect("interrupt");
    let manager = RunManager::new(storage).expect("manager");

    let error = manager
        .resume_run("done", ExplorerPrompt::new("again"))
        .await
        .expect_err("a completed run is not resumable");
    assert!(
        error.message.contains("cannot be resumed"),
        "{}",
        error.message
    );

    let error = manager
        .resume_run("no-session", ExplorerPrompt::new("again"))
        .await
        .expect_err("a run with no ACP session is not resumable");
    assert!(
        error.message.contains("no ACP session"),
        "{}",
        error.message
    );

    let error = manager
        .resume_run("absent", ExplorerPrompt::new("again"))
        .await
        .expect_err("an unknown run is not resumable");
    assert!(error.message.contains("not found"), "{}", error.message);
}

#[tokio::test]
async fn a_second_resume_cannot_reopen_a_run_that_is_already_active() {
    // The registration is the gate. A resume that lost the race used to
    // reopen the winner's row on its way out, and a cancel landing in
    // the same gap had no registration to record its intent on.
    let path = temp_db_path("resume-active");
    let storage = open_storage(&path);
    storage
        .create_work_run("active", WORKTREE, "summarize src/")
        .expect("create");
    storage
        .update_work_run(
            "active",
            &super::store::WorkRunUpdate {
                status: Some("interrupted".to_owned()),
                acp_session_id: Some("fake-session-1".to_owned()),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .expect("interrupt");
    let manager = RunManager::new(storage).expect("manager");
    let reopened = manager
        .resume_run("active", ExplorerPrompt::new("continue"))
        .await
        .expect("first resume accepted");
    assert_eq!(reopened.status, "pending");
    assert!(manager.is_in_flight("active"));

    let error = manager
        .resume_run("active", ExplorerPrompt::new("continue again"))
        .await
        .expect_err("an active run cannot be resumed twice");
    assert!(
        error.message.contains("already active"),
        "{}",
        error.message
    );
    // The live row is untouched by the refused resume.
    let row = manager.load("active").unwrap().expect("row");
    assert_eq!(row.status, "pending", "the winner keeps its own row");
    assert!(!row.finished_at.is_some());
    assert!(manager.is_in_flight("active"));
}

#[tokio::test]
async fn a_resume_that_is_refused_retires_its_registration() {
    // A refusal after the gate was taken must not leave a registration
    // nothing will ever settle, or the run id stays permanently active.
    let path = temp_db_path("resume-refused-gate");
    let storage = open_storage(&path);
    storage
        .create_work_run("done", WORKTREE, "p")
        .expect("create");
    storage
        .update_work_run(
            "done",
            &super::store::WorkRunUpdate {
                status: Some("completed".to_owned()),
                acp_session_id: Some("ses-1".to_owned()),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .expect("complete");
    let manager = RunManager::new(storage).expect("manager");
    let error = manager
        .resume_run("done", ExplorerPrompt::new("again"))
        .await
        .expect_err("a completed run is not resumable");
    assert!(
        error.message.contains("cannot be resumed"),
        "{}",
        error.message
    );
    assert!(
        !manager.is_in_flight("done"),
        "the refused resume must not hold the run id"
    );
    let row = manager.load("done").unwrap().expect("row");
    assert_eq!(row.status, "completed", "the refused row is untouched");
}

#[tokio::test]
async fn a_resumed_run_keeps_its_own_worktree_server_side() {
    // The worktree path must stay in the ledger and out of anything a
    // model can read, including after a resume reopens the row.
    let path = temp_db_path("resume-server-side");
    let storage = open_storage(&path);
    storage
        .create_work_run("private", "/secret/wt-a", "p")
        .expect("create");
    storage
        .update_work_run(
            "private",
            &super::store::WorkRunUpdate {
                status: Some("interrupted".to_owned()),
                acp_session_id: Some("ses-1".to_owned()),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .expect("interrupt");
    let reopened = storage.resume_work_run("private").expect("resume");
    assert_eq!(reopened.worktree_cwd, "/secret/wt-a");
    let json = serde_json::to_value(&reopened).expect("serialize");
    assert!(json.get("worktree_cwd").is_none());
    assert!(
        !format!("{reopened:?}").contains("/secret/wt-a"),
        "the Debug output must not carry the worktree path"
    );
}

#[tokio::test]
async fn an_interrupted_run_is_not_left_running_after_a_resumed_attempt_fails() {
    let path = temp_db_path("resume-failure");
    let storage = open_storage(&path);
    storage
        .create_work_run("failing", WORKTREE, "p")
        .expect("create");
    storage
        .update_work_run(
            "failing",
            &super::store::WorkRunUpdate {
                status: Some("interrupted".to_owned()),
                acp_session_id: Some("fake-session-1".to_owned()),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .expect("interrupt");
    // The persisted session id is well formed but the real agent is
    // not reachable, so the resumed run lands on a terminal status
    // rather than staying open.
    let manager = RunManager::new(storage).expect("manager");
    manager
        .resume_run("failing", ExplorerPrompt::new("continue"))
        .await
        .expect("resume accepted");
    let reader = open_storage(&path);
    let settled = tokio::time::timeout(Duration::from_secs(30), wait_terminal(&reader, "failing"))
        .await
        .expect("the resumed attempt settles");
    assert!(
        settled.status != "pending" && settled.status != "running",
        "a resumed attempt must reach a terminal status, got {}",
        settled.status
    );
}
