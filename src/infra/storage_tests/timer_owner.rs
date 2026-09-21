use super::support::*;
use super::*;

#[test]
fn owner_metadata_round_trips_and_validates_bounds() {
    let (temp_dir, storage) = open_at_temp("owner-metadata");
    let run = storage
        .start_timer_run_with_owner(
            "owner-1",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-123".to_owned()),
                call_id: Some("call-abc".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(run.owner_session_id.as_deref(), Some("sess-123"));
    assert_eq!(run.owner_call_id.as_deref(), Some("call-abc"));

    // Empty strings collapse to NULL so the column stores NULL instead
    // of an empty marker.
    let blank = storage
        .start_timer_run_with_owner(
            "owner-2",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_001,
            &TimerRunOwner {
                session_id: Some("   ".to_owned()),
                call_id: None,
            },
        )
        .unwrap();
    assert!(blank.owner_session_id.is_none());
    assert!(blank.owner_call_id.is_none());

    // Control characters and oversize values are rejected before the
    // row touches the database.
    let control = storage
        .start_timer_run_with_owner(
            "owner-3",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_002,
            &TimerRunOwner {
                session_id: Some("a\nb".to_owned()),
                call_id: None,
            },
        )
        .unwrap_err();
    assert!(control.contains("control characters"));

    let oversize = "x".repeat(200);
    let oversize_err = storage
        .start_timer_run_with_owner(
            "owner-4",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_003,
            &TimerRunOwner {
                session_id: Some(oversize),
                call_id: None,
            },
        )
        .unwrap_err();
    assert!(oversize_err.contains("at most 128"));

    // Existing legacy start_timer_run callers continue to leave the
    // owner columns null so old test code keeps compiling and running.
    let legacy = storage
        .start_timer_run(
            "owner-5",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_004,
        )
        .unwrap();
    assert!(legacy.owner_session_id.is_none());
    assert!(legacy.owner_call_id.is_none());
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn owner_mismatch_on_repeated_start_is_rejected() {
    // Two competing calls for the same run id must not silently overwrite
    // an owner that was already attached by another session.
    let (temp_dir, storage) = open_at_temp("owner-mismatch");
    storage
        .start_timer_run_with_owner(
            "owner-race",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-A".to_owned()),
                call_id: None,
            },
        )
        .unwrap();
    let error = storage
        .start_timer_run_with_owner(
            "owner-race",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-B".to_owned()),
                call_id: None,
            },
        )
        .unwrap_err();
    assert!(error.contains("already owned by another session"));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn list_timer_runs_groups_running_and_finished_with_clamped_limit() {
    let (temp_dir, storage) = open_at_temp("list-runs");
    for index in 0..3 {
        let run_id = format!("run-{index}");
        storage
            .start_timer_run(
                &run_id,
                28,
                "implementation",
                "executor",
                1,
                1_700_000_000 + index,
            )
            .unwrap();
    }
    storage
        .finish_timer_run("run-0", "DONE", 1_700_000_010)
        .unwrap();

    let running = storage
        .list_timer_runs(TimerStatusFilter::Running, 100)
        .unwrap();
    assert_eq!(running.len(), 2);
    assert!(running.iter().all(|run| run.status == "running"));

    let finished = storage
        .list_timer_runs(TimerStatusFilter::Finished, 100)
        .unwrap();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].run_id, "run-0");
    assert_eq!(finished[0].status, "DONE");

    let all = storage
        .list_timer_runs(TimerStatusFilter::All, 100)
        .unwrap();
    assert_eq!(all.len(), 3);

    // Limit 0 is clamped up to 1 so the caller always gets at least one
    // candidate row when one exists.
    let one = storage.list_timer_runs(TimerStatusFilter::All, 0).unwrap();
    assert_eq!(one.len(), 1);
    // Limits above the cap clamp down without an error.
    let many = storage
        .list_timer_runs(TimerStatusFilter::All, 10_000)
        .unwrap();
    assert_eq!(many.len(), 3);
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn additive_owner_migration_is_idempotent_across_reopens() {
    // Additive ALTER TABLE statements add the owner columns. Opening an
    // already-migrated database must not error (column_exists returns
    // true and the ALTER is skipped) and a row written before the
    // migration must remain readable.
    let (temp_dir, storage) = open_at_temp("owner-migration");
    storage
        .start_timer_run(
            "pre-migration",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
        )
        .unwrap();

    let reopened = Storage::open_at(&temp_dir.join(DB_FILENAME)).unwrap();
    let pre = reopened.load_timer_run("pre-migration").unwrap().unwrap();
    assert!(pre.owner_session_id.is_none());
    assert!(pre.owner_call_id.is_none());

    reopened
        .start_timer_run_with_owner(
            "post-migration",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_001,
            &TimerRunOwner {
                session_id: Some("sess-2".to_owned()),
                call_id: None,
            },
        )
        .unwrap();
    let post = reopened.load_timer_run("post-migration").unwrap().unwrap();
    assert_eq!(post.owner_session_id.as_deref(), Some("sess-2"));
    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn owner_call_mismatch_on_repeated_start_is_rejected() {
    let (temp_dir, storage) = open_at_temp("owner-call-mismatch");
    storage
        .start_timer_run_with_owner(
            "owner-call-race",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-A".to_owned()),
                call_id: Some("call-1".to_owned()),
            },
        )
        .unwrap();
    let err = storage
        .start_timer_run_with_owner(
            "owner-call-race",
            28,
            "implementation",
            "executor",
            1,
            1_700_000_000,
            &TimerRunOwner {
                session_id: Some("sess-A".to_owned()),
                call_id: Some("call-2".to_owned()),
            },
        )
        .unwrap_err();
    assert!(err.contains("already owned by another call"));
    let _ = fs::remove_dir_all(temp_dir);
}
