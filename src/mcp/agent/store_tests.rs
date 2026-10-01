//! Tests for the `acp_explorer_runs` ledger methods on `Storage`.
//!
//! The work-run persistence is part of the explorer agent surface, so
//! its storage tests live beside the adapter that owns the table
//! rather than in the generic storage test module.

use super::store::WorkRunUpdate;
use crate::infra::storage::{DB_FILENAME, Storage};

pub(crate) fn temp_db_path(label: &str) -> std::path::PathBuf {
    let dir = crate::test_scratch::root().join(format!(
        "phasegent-work-runs-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    dir.join(DB_FILENAME)
}

pub(crate) fn open_storage(label: &str) -> Storage {
    Storage::open_at(&temp_db_path(label)).expect("temp storage")
}

pub(crate) fn sample_prompt() -> String {
    "list the modules in src/ and summarize entry points".to_owned()
}

#[test]
fn work_run_round_trips_through_create_load_update() {
    let storage = open_storage("round-trip");
    let created = storage
        .create_work_run("run-1", "/tmp/wt-a", &sample_prompt())
        .expect("create run");
    assert_eq!(created.status, "pending");
    assert_eq!(created.worktree_cwd, "/tmp/wt-a");
    assert!(created.acp_session_id.is_none());
    assert!(created.output.is_none());
    assert!(!created.output_truncated);
    assert!(created.finished_at.is_none());

    let updated = storage
        .update_work_run(
            "run-1",
            &WorkRunUpdate {
                status: Some("running".to_owned()),
                acp_session_id: Some("ses-9".to_owned()),
                ..WorkRunUpdate::default()
            },
        )
        .expect("mark running");
    assert_eq!(updated.status, "running");
    assert_eq!(updated.acp_session_id.as_deref(), Some("ses-9"));
    assert!(updated.created_at <= updated.updated_at);

    let finished = storage
        .update_work_run(
            "run-1",
            &WorkRunUpdate {
                status: Some("completed".to_owned()),
                output: Some("src has 12 modules".to_owned()),
                output_truncated: Some(true),
                finished_at: Some(1_800_000_000),
                ..WorkRunUpdate::default()
            },
        )
        .expect("finish run");
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.output.as_deref(), Some("src has 12 modules"));
    assert!(finished.output_truncated);
    assert_eq!(finished.finished_at, Some(1_800_000_000));

    let loaded = storage.load_work_run("run-1").unwrap().unwrap();
    assert_eq!(loaded.run_id, "run-1");
    assert_eq!(loaded.acp_session_id.as_deref(), Some("ses-9"));
}

#[test]
fn duplicate_run_ids_and_invalid_rows_are_rejected() {
    let storage = open_storage("duplicates");
    storage
        .create_work_run("dup", "/tmp/wt", &sample_prompt())
        .expect("first insert");
    let error = storage
        .create_work_run("dup", "/tmp/wt", &sample_prompt())
        .expect_err("duplicate run id must be rejected");
    assert!(error.contains("already exists"), "{error}");

    assert!(
        storage
            .create_work_run("", "/tmp/wt", &sample_prompt())
            .is_err()
    );
    assert!(
        storage
            .create_work_run("bad\x01id", "/tmp/wt", &sample_prompt())
            .is_err()
    );
    assert!(storage.create_work_run("ok", "", &sample_prompt()).is_err());
    assert!(storage.create_work_run("ok", "/tmp/wt", "  ").is_err());
    let long_prompt = "x".repeat(super::MAX_TRANSCRIPT_CHARS + 1);
    assert!(
        storage
            .create_work_run("ok", "/tmp/wt", &long_prompt)
            .is_err()
    );
}

#[test]
fn serialized_run_never_carries_the_worktree_path() {
    let storage = open_storage("serialize");
    let run = storage
        .create_work_run("s1", "/secret/worktree/wt-a", &sample_prompt())
        .unwrap();
    let json = serde_json::to_value(&run).unwrap();
    assert!(
        json.get("worktree_cwd").is_none(),
        "cwd must stay server-side"
    );
    assert_eq!(json["run_id"], "s1");
    assert_eq!(json["status"], "pending");
}

#[test]
fn terminal_runs_never_transition_again() {
    let storage = open_storage("terminal");
    storage
        .create_work_run("t1", "/tmp/wt", &sample_prompt())
        .unwrap();
    storage
        .update_work_run(
            "t1",
            &WorkRunUpdate {
                status: Some("failed".to_owned()),
                error: Some("spawn".to_owned()),
                ..WorkRunUpdate::default()
            },
        )
        .unwrap();
    let error = storage
        .update_work_run(
            "t1",
            &WorkRunUpdate {
                status: Some("completed".to_owned()),
                ..WorkRunUpdate::default()
            },
        )
        .expect_err("terminal rows are frozen");
    assert!(error.contains("already failed"), "{error}");
    assert!(
        storage
            .update_work_run(
                "t1",
                &WorkRunUpdate {
                    status: Some("flying".to_owned()),
                    ..WorkRunUpdate::default()
                },
            )
            .is_err()
    );
    assert!(storage.load_work_run("missing").unwrap().is_none());
}

#[test]
fn run_listing_supports_the_recovery_scan() {
    let storage = open_storage("listing");
    for (id, status) in [
        ("a", "completed"),
        ("b", "running"),
        ("c", "pending"),
        ("d", "cancelled"),
    ] {
        storage
            .create_work_run(id, "/tmp/wt", &sample_prompt())
            .unwrap();
        if status != "pending" {
            storage
                .update_work_run(
                    id,
                    &WorkRunUpdate {
                        status: Some(status.to_owned()),
                        ..WorkRunUpdate::default()
                    },
                )
                .unwrap();
        }
    }
    let all = storage.list_work_runs(false, 100).unwrap();
    assert_eq!(all.len(), 4);
    let open: Vec<String> = storage
        .list_work_runs(true, 100)
        .unwrap()
        .into_iter()
        .map(|run| run.run_id)
        .collect();
    assert!(open.contains(&"b".to_owned()));
    assert!(open.contains(&"c".to_owned()));
    assert!(!open.contains(&"a".to_owned()));
    assert!(!open.contains(&"d".to_owned()));
    let bounded = storage.list_work_runs(false, 2).unwrap();
    assert_eq!(bounded.len(), 2);
}

#[test]
fn a_run_debug_view_never_prints_the_worktree_path() {
    let storage = open_storage("debug");
    let run = storage
        .create_work_run("d1", "/secret/wt", &sample_prompt())
        .expect("create");
    let rendered = format!("{run:?}");
    assert!(rendered.contains("d1"));
    assert!(
        !rendered.contains("/secret/wt"),
        "Debug must omit the worktree path: {rendered}"
    );
}
