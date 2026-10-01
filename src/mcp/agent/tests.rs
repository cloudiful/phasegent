//! Adapter tests for the MCode ACP process/session surface.
//!
//! Spawn-level cases use a missing program name so no real `mcode`
//! binary, credential, or network is involved; protocol-level cases
//! live in [`super::protocol_tests`] against the in-process fake agent
//! in [`super::test_kit`].

use std::path::Path;

use super::session::AcpSession;
use super::types::{AcpSpawnConfig, ExplorerPrompt, MAX_TRANSCRIPT_CHARS, Transcript};
use super::wire;

#[tokio::test]
async fn spawn_rejects_a_missing_program() {
    let config = AcpSpawnConfig {
        program: "phasegent-definitely-not-a-binary".to_owned(),
        cwd: std::env::temp_dir(),
        handshake_timeout_secs: None,
    };
    let error = match AcpSession::start(&config).await {
        Err(error) => error,
        Ok(_) => panic!("a missing program must fail at spawn"),
    };
    assert_eq!(error.kind.as_str(), "spawn");
}

#[test]
fn transcript_bounds_and_marks_truncation() {
    let mut transcript = Transcript::default();
    transcript.push_chunk(&"a".repeat(MAX_TRANSCRIPT_CHARS));
    assert!(!transcript.truncated());
    transcript.push_chunk("tail");
    assert!(transcript.truncated());
    assert_eq!(transcript.text().chars().count(), MAX_TRANSCRIPT_CHARS);
    assert!(transcript.text().ends_with("tail"));
}

#[test]
fn permission_decisions_follow_the_read_only_contract() {
    // `search` is the explorer's primary tool; a read-only explorer
    // that cannot search is useless.
    for kind in ["read", "search", "fetch"] {
        assert_eq!(
            super::permission_decision_for_kind(kind),
            super::PermissionDecision::Allow,
            "kind {kind:?} is read-only"
        );
    }
    for kind in [
        "edit",
        "delete",
        "move",
        "execute",
        "think",
        "switch_mode",
        "other",
        "",
        "unknown",
    ] {
        assert_eq!(
            super::permission_decision_for_kind(kind),
            super::PermissionDecision::Deny,
            "kind {kind:?} must be denied"
        );
    }
}

#[test]
fn timeout_bounds_clamp() {
    let prompt = ExplorerPrompt::new("x").with_timeout_secs(999_999);
    assert_eq!(
        prompt.timeout_secs,
        Some(super::types::MAX_PROMPT_TIMEOUT_SECS)
    );
    let prompt = ExplorerPrompt::new("x").with_timeout_secs(0);
    assert_eq!(prompt.timeout_secs, Some(1));
    assert_eq!(
        ExplorerPrompt::new("x").timeout_secs,
        None,
        "default timeout stays unset until applied"
    );
}

#[test]
fn spawn_config_defaults_to_mcode_acp() {
    let config = AcpSpawnConfig::new(Path::new("/tmp/wt"));
    assert_eq!(config.program, "mcode");
    assert_eq!(config.cwd, Path::new("/tmp/wt"));
    assert_eq!(
        wire::EXPLORER_MODEL_WIRE_VALUE,
        "m:minimax:MiniMax-M3.1-Flash-Preview:v:thinking"
    );
    assert_eq!(wire::EXPLORER_THINKING_EFFORT, "high");
}

#[test]
fn error_sanitization_never_carries_control_characters() {
    let error = super::error::AgentError::spawn("boom\nwith\x07controls");
    assert!(!error.message.chars().any(char::is_control));
}

// --- RunManager durable lifecycle -----------------------------------------

pub(crate) fn temp_db_path(label: &str) -> std::path::PathBuf {
    scratch_dir(label).join(crate::infra::storage::DB_FILENAME)
}

/// A fresh, empty scratch directory under the test scratch root, for
/// cases that need real directories or links rather than a database.
pub(crate) fn scratch_dir(label: &str) -> std::path::PathBuf {
    let dir = crate::test_scratch::root().join(format!(
        "phasegent-agent-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create test scratch dir");
    dir
}

pub(crate) fn open_storage(path: &std::path::Path) -> crate::infra::storage::Storage {
    crate::infra::storage::Storage::open_at(path).expect("storage")
}

pub(crate) async fn wait_terminal(
    storage: &crate::infra::storage::Storage,
    run_id: &str,
) -> super::store::WorkRun {
    for _ in 0..200 {
        let run = storage.load_work_run(run_id).unwrap().expect("run row");
        if super::store::TERMINAL_STATUSES.contains(&run.status.as_str()) {
            return run;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("run {run_id} never reached a terminal status");
}

#[test]
fn recover_marks_orphaned_runs_interrupted() {
    let path = temp_db_path("recover");
    let storage = open_storage(&path);
    storage
        .create_work_run("orphan-1", "/tmp/wt", "prompt")
        .unwrap();
    storage
        .update_work_run(
            "orphan-1",
            &super::store::WorkRunUpdate {
                status: Some("running".to_owned()),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .unwrap();
    storage
        .create_work_run("done-1", "/tmp/wt", "prompt")
        .unwrap();
    storage
        .update_work_run(
            "done-1",
            &super::store::WorkRunUpdate {
                status: Some("completed".to_owned()),
                finished_at: Some(1),
                ..super::store::WorkRunUpdate::default()
            },
        )
        .unwrap();

    let manager = super::RunManager::new(storage).expect("manager");
    assert_eq!(
        manager.recover().unwrap(),
        0,
        "a second recovery is a no-op"
    );

    let reader = open_storage(&path);
    let orphan = reader.load_work_run("orphan-1").unwrap().unwrap();
    assert_eq!(orphan.status, "interrupted");
    assert!(orphan.error.unwrap().contains("restarted"));
    let done = reader.load_work_run("done-1").unwrap().unwrap();
    assert_eq!(done.status, "completed", "terminal rows are untouched");
}

#[tokio::test]
async fn start_run_persists_failure_when_the_process_cannot_spawn() {
    let path = temp_db_path("spawn-failure");
    let manager = super::RunManager::new(open_storage(&path)).expect("manager");
    let config = super::AcpSpawnConfig {
        program: "phasegent-definitely-not-a-binary".to_owned(),
        cwd: std::env::temp_dir(),
        handshake_timeout_secs: None,
    };
    let created = manager
        .start_run(
            "spawn-fail",
            "ses_spawn_fail",
            config,
            ExplorerPrompt::new("go"),
        )
        .await
        .expect("run accepted");
    assert_eq!(created.status, "pending");

    let reader = open_storage(&path);
    let finished = wait_terminal(&reader, "spawn-fail").await;
    assert_eq!(finished.status, "failed");
    assert!(finished.error.unwrap().contains("spawn"));
    for _ in 0..80 {
        if !manager.is_in_flight("spawn-fail") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(!manager.is_in_flight("spawn-fail"));
}

#[tokio::test]
async fn cancel_run_rejects_unknown_ids_and_returns_known_rows() {
    let path = temp_db_path("cancel");
    let storage = open_storage(&path);
    storage
        .create_work_run("known", "/tmp/wt", "prompt")
        .unwrap();
    let manager = super::RunManager::new(storage).expect("manager");
    let error = manager
        .cancel_run("missing")
        .await
        .expect_err("an unknown run cannot be cancelled");
    assert!(error.contains("was not found"), "{error}");
    // Construction recovered the orphaned `pending` row as
    // interrupted, so cancelling an inactive run is a read-only no-op.
    let row = manager.cancel_run("known").await.expect("known run");
    assert_eq!(row.run_id, "known");
    assert_eq!(row.status, "interrupted");
    assert!(manager.live_session("known").is_none());
    assert!(!manager.is_cancelled("known"));
}
