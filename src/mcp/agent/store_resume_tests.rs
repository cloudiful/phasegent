//! Ledger tests for reopening an explorer run on its ACP session.
//!
//! `resume_work_run` is the single, explicit exception to the terminal
//! freeze the rest of the ledger enforces, so its boundaries get their
//! own module: which statuses may reopen, what survives the reopen, and
//! what a refused reopen must leave untouched.

use super::store::WorkRunUpdate;

use super::store_tests::{open_storage, sample_prompt};

#[test]
fn resume_reopens_only_resumable_rows_and_keeps_their_session_id() {
    let storage = open_storage("resume");
    for (id, status) in [
        ("interrupted", "interrupted"),
        ("timed-out", "timed_out"),
        ("failed", "failed"),
        ("cancelled", "cancelled"),
        ("completed", "completed"),
        ("live", "running"),
    ] {
        storage
            .create_work_run(id, "/tmp/wt", &sample_prompt())
            .expect("create");
        storage
            .update_work_run(
                id,
                &WorkRunUpdate {
                    status: Some(status.to_owned()),
                    acp_session_id: Some("ses-1".to_owned()),
                    error: Some("previous".to_owned()),
                    finished_at: Some(1_800_000_000),
                    ..WorkRunUpdate::default()
                },
            )
            .expect("update");
    }
    // A resumable row goes back to `pending` and loses only its
    // terminal markers; its ACP session id and prompt survive.
    for id in ["interrupted", "timed-out", "failed", "cancelled"] {
        let resumed = storage.resume_work_run(id).expect("resume");
        assert_eq!(resumed.status, "pending", "{id} must reopen");
        assert_eq!(resumed.acp_session_id.as_deref(), Some("ses-1"));
        assert_eq!(resumed.prompt, sample_prompt());
        assert!(resumed.error.is_none(), "{id} keeps no stale error");
        assert!(resumed.finished_at.is_none(), "{id} has no finish time");
        // A reopened row is not terminal again, so the generic update
        // path can move it.
        assert!(
            storage
                .update_work_run(
                    id,
                    &WorkRunUpdate {
                        status: Some("completed".to_owned()),
                        ..WorkRunUpdate::default()
                    },
                )
                .is_ok(),
            "{id} is a fresh pending row"
        );
    }
    for id in ["completed", "live"] {
        let error = storage
            .resume_work_run(id)
            .expect_err("only terminal resumable rows reopen");
        assert!(error.contains("cannot be resumed"), "{id}: {error}");
    }
}

#[test]
fn a_run_without_an_acp_session_cannot_be_resumed() {
    let storage = open_storage("resume-no-session");
    storage
        .create_work_run("no-session", "/tmp/wt", &sample_prompt())
        .expect("create");
    storage
        .update_work_run(
            "no-session",
            &WorkRunUpdate {
                status: Some("interrupted".to_owned()),
                ..WorkRunUpdate::default()
            },
        )
        .expect("update");
    let error = storage
        .resume_work_run("no-session")
        .expect_err("there is nothing to reconnect to");
    assert!(error.contains("no ACP session"), "{error}");
    assert!(storage.resume_work_run("absent").is_err());
    // The refused row is untouched.
    let row = storage.load_work_run("no-session").unwrap().unwrap();
    assert_eq!(row.status, "interrupted");
}
