//! Unit tests of the bounded diagnostic log.
//!
//! Every test works in its own directory under the system temporary
//! directory, so no test reads or writes the real log directory and no test
//! needs a database, a network, or a process fixture.

use std::cell::Cell;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::event::EventKind;
use super::query::{LogQuery, query_in};
use super::writer::{
    ACTIVE_LOG, ARCHIVE_COUNT, AppendError, DiagLogger, LOCK_FILE, MAX_FILE_BYTES, append_line,
    apply_retention, archive_name, rotate,
};

thread_local! {
    /// The warnings this thread's test observed. The counter is per thread, so
    /// the parallel test run never mixes two tests' warnings.
    static WARNINGS: Cell<usize> = const { Cell::new(0) };
}

/// The bounded warning sink the tests observe.
fn count_warning(_message: &str) {
    WARNINGS.with(|counter| counter.set(counter.get() + 1));
}

/// The number of warnings this test observed so far.
fn warnings() -> usize {
    WARNINGS.with(Cell::get)
}

/// A fresh, empty directory for one test.
fn test_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "phasegent-diaglog-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("the test directory is created");
    path
}

/// Write one file with the given content and modification time.
fn write_file(path: &Path, content: &str, age: Duration) {
    fs::write(path, content).expect("the test file is written");
    let file = File::open(path).expect("the test file is opened");
    file.set_modified(SystemTime::now() - age)
        .expect("the test file is stamped");
}

/// Write one file with the given content and a future modification time.
fn write_future_file(path: &Path, content: &str) {
    fs::write(path, content).expect("the test file is written");
    let file = File::open(path).expect("the test file is opened");
    file.set_modified(SystemTime::now() + Duration::from_secs(60 * 60))
        .expect("the test file is stamped");
}

/// The typed events of one directory, newest last.
fn events(dir: &Path, run: Option<&str>) -> super::query::LogQueryResult {
    query_in(
        dir,
        &LogQuery {
            run: run.map(str::to_owned),
            limit: 500,
        },
    )
}

#[test]
fn a_typed_event_round_trips_through_the_log() {
    let dir = test_dir("round-trip");
    let logger = DiagLogger::open_in(&dir, "remove", count_warning).expect("the logger opens");
    assert_eq!(logger.mode(), "remove", "the logger knows its own mode");
    assert_eq!(
        logger.directory(),
        dir,
        "the logger knows its own directory"
    );
    logger.event(EventKind::RunStart).write();
    logger.event(EventKind::StageStart).stage("scan").write();
    logger
        .event(EventKind::StageEnd)
        .stage("scan")
        .duration_ms(12.5)
        .calls(1)
        .write();

    let result = events(&dir, None);
    assert_eq!(result.entries.len(), 3, "every written event is readable");
    assert_eq!(result.entries[0].kind, EventKind::RunStart);
    assert_eq!(result.entries[1].stage.as_deref(), Some("scan"));
    assert_eq!(result.entries[2].duration_ms, Some(12.5));
    assert_eq!(result.entries[2].calls, Some(1));
    assert_eq!(result.skipped_malformed, 0);
    assert!(!result.busy);
    assert_eq!(result.directory, dir);
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

#[test]
fn every_event_kind_has_its_stable_name() {
    for (kind, name) in [
        (EventKind::RunStart, "run_start"),
        (EventKind::RunEnd, "run_end"),
        (EventKind::StageStart, "stage_start"),
        (EventKind::StageEnd, "stage_end"),
        (EventKind::OperationStart, "operation_start"),
        (EventKind::OperationEnd, "operation_end"),
        (EventKind::CandidateResult, "candidate_result"),
    ] {
        assert_eq!(kind.as_str(), name, "the kind {kind:?} keeps its name");
    }
}

#[test]
fn an_unknown_or_unsafe_field_is_rejected_and_never_echoed() {
    let dir = test_dir("unsafe");
    let logger = DiagLogger::open_in(&dir, "remove", count_warning).expect("the logger opens");
    logger.event(EventKind::RunStart).write();
    // A hand-written line carries a path and a raw error: the schema does not
    // describe them, so the line is malformed and is never echoed.
    let unsafe_line = serde_json::json!({
        "schema_version": 1,
        "run_id": "run-1",
        "seq": 1,
        "ts_ms": 1,
        "elapsed_ms": 0.0,
        "command": "worktree_prune",
        "mode": "remove",
        "kind": "run_end",
        "path": "/home/user/secret",
        "error": "connection refused to the provider"
    })
    .to_string();
    let active = dir.join(ACTIVE_LOG);
    let mut content = fs::read_to_string(&active).expect("the active log is readable");
    content.push_str(&unsafe_line);
    content.push('\n');
    fs::write(&active, content).expect("the active log is written");

    let result = events(&dir, None);
    assert_eq!(result.entries.len(), 1, "only the typed event is readable");
    assert_eq!(result.skipped_malformed, 1, "the unsafe line is counted");
    let rendered = serde_json::to_string(&result.entries).expect("the entries render");
    assert!(!rendered.contains("secret"), "no path may be echoed");
    assert!(
        !rendered.contains("connection refused"),
        "no raw error may be echoed"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

#[test]
fn rotation_shifts_the_archives_and_keeps_the_newest_first() {
    let dir = test_dir("rotation");
    for index in 1..=ARCHIVE_COUNT {
        write_file(
            &dir.join(archive_name(index)),
            &format!("archive {index}"),
            Duration::ZERO,
        );
    }
    write_file(&dir.join(ACTIVE_LOG), "active", Duration::ZERO);

    rotate(&dir).expect("the rotation succeeds");

    assert!(
        !dir.join(ACTIVE_LOG).exists(),
        "the active file is rotated away"
    );
    assert_eq!(
        fs::read_to_string(dir.join(archive_name(1))).expect("the newest archive is readable"),
        "active"
    );
    assert_eq!(
        fs::read_to_string(dir.join(archive_name(2))).expect("the second archive is readable"),
        "archive 1"
    );
    assert!(
        !dir.join(archive_name(ARCHIVE_COUNT + 1)).exists(),
        "no archive beyond the fixed count is created"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

#[test]
fn the_daily_rollover_rotates_an_older_active_file_only() {
    let dir = test_dir("daily");
    write_file(
        &dir.join(ACTIVE_LOG),
        "yesterday",
        Duration::from_secs(48 * 60 * 60),
    );
    append_line(&dir, b"today\n").expect("the append succeeds");
    assert_eq!(
        fs::read_to_string(dir.join(ACTIVE_LOG)).expect("the active log is readable"),
        "today\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join(archive_name(1))).expect("the archive is readable"),
        "yesterday"
    );

    // A future timestamp is not an earlier day, so it never forces a rollover.
    let fresh = test_dir("daily-future");
    write_future_file(&fresh.join(ACTIVE_LOG), "future");
    append_line(&fresh, b"later\n").expect("the append succeeds");
    assert!(
        !fresh.join(archive_name(1)).exists(),
        "a future timestamp must not rotate the log"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
    fs::remove_dir_all(&fresh).expect("the test directory is removed");
}

#[test]
fn the_size_cap_keeps_a_line_that_does_not_fit_out_of_the_log() {
    let dir = test_dir("cap");
    let oversized = vec![b'x'; MAX_FILE_BYTES as usize + 1];
    assert!(
        append_line(&dir, &oversized).is_err(),
        "a line beyond the file cap is refused"
    );
    assert!(
        !dir.join(ACTIVE_LOG).exists(),
        "the refused line leaves no file behind"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

#[test]
fn the_retention_sweep_removes_only_the_owned_aged_files() {
    let dir = test_dir("retention");
    let old = Duration::from_secs(20 * 24 * 60 * 60);
    write_file(&dir.join(ACTIVE_LOG), "recent", Duration::ZERO);
    write_file(&dir.join(archive_name(1)), "old", old);
    write_file(&dir.join("unrelated.txt"), "keep", old);
    #[cfg(unix)]
    {
        let link = dir.join(archive_name(2));
        std::os::unix::fs::symlink(dir.join("unrelated.txt"), &link).expect("the link is created");
    }

    apply_retention(&dir).expect("the retention sweep succeeds");

    assert!(dir.join(ACTIVE_LOG).exists(), "a recent file is kept");
    assert!(
        !dir.join(archive_name(1)).exists(),
        "an aged owned file is removed"
    );
    assert!(
        dir.join("unrelated.txt").exists(),
        "an unrelated file is never swept"
    );
    #[cfg(unix)]
    assert!(
        dir.join(archive_name(2)).exists(),
        "a symbolic link is never swept"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

#[test]
fn a_busy_lock_drops_the_event_and_warns_once_per_run() {
    let dir = test_dir("busy-writer");
    let logger = DiagLogger::open_in(&dir, "remove", count_warning).expect("the logger opens");
    let lock = File::open(dir.join(LOCK_FILE)).expect("the lock file is opened");
    lock.try_lock().expect("the test holds the lock");
    let before = warnings();

    assert!(
        !logger.event(EventKind::RunStart).write(),
        "the event is dropped"
    );
    assert!(
        !logger.event(EventKind::RunEnd).write(),
        "the event is dropped"
    );
    assert_eq!(
        warnings() - before,
        1,
        "a busy lock reports exactly one bounded warning per run"
    );
    drop(lock);
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

#[test]
fn the_query_of_a_missing_directory_is_an_empty_success() {
    let dir = test_dir("missing");
    let missing = dir.join("absent");
    let result = query_in(
        &missing,
        &LogQuery {
            run: None,
            limit: 50,
        },
    );
    assert!(
        result.entries.is_empty(),
        "a missing log directory is empty"
    );
    assert!(!result.busy, "a missing log directory is not busy");
    assert!(
        !missing.exists(),
        "the query creates no directory, file, or lock"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

#[test]
fn a_busy_writer_makes_the_query_report_busy_without_entries() {
    let dir = test_dir("busy-query");
    let logger = DiagLogger::open_in(&dir, "remove", count_warning).expect("the logger opens");
    logger.event(EventKind::RunStart).write();
    let lock = File::open(dir.join(LOCK_FILE)).expect("the lock file is opened");
    lock.try_lock().expect("the test holds the lock");

    let result = events(&dir, None);
    assert!(result.busy, "the query reports the busy lock");
    assert!(result.entries.is_empty(), "a busy query returns no entries");

    drop(lock);
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

#[test]
fn the_query_filters_by_the_exact_run_identifier() {
    let dir = test_dir("filter");
    let first = DiagLogger::open_in(&dir, "remove", count_warning).expect("the logger opens");
    let first_run = first.run_id().to_owned();
    first.event(EventKind::RunStart).write();
    let second = DiagLogger::open_in(&dir, "dry_run", count_warning).expect("the logger opens");
    let second_run = second.run_id().to_owned();
    second.event(EventKind::RunStart).write();

    assert_ne!(first_run, second_run, "every run has its own identifier");
    let filtered = events(&dir, Some(&first_run));
    assert_eq!(filtered.entries.len(), 1, "only the named run is returned");
    assert_eq!(filtered.entries[0].run_id, first_run);
    assert!(
        events(&dir, Some("run-does-not-exist")).entries.is_empty(),
        "an unknown run identifier returns nothing"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// A rollover never replaces an unsafe destination. A symbolic link in an
/// archive slot makes the rotation fail closed: the link is still there, the
/// active file is untouched, and no archive was shifted.
#[cfg(unix)]
#[test]
fn a_rollover_into_a_symbolic_link_fails_closed() {
    let dir = test_dir("rotate-link");
    let target = dir.join("elsewhere.txt");
    write_file(&target, "untouched", Duration::ZERO);
    std::os::unix::fs::symlink(&target, dir.join(archive_name(1))).expect("the link is created");
    write_file(
        &dir.join(ACTIVE_LOG),
        "yesterday",
        Duration::from_secs(48 * 60 * 60),
    );

    assert!(
        append_line(&dir, b"today\n").is_err(),
        "the rotation must fail closed"
    );
    let link = fs::symlink_metadata(dir.join(archive_name(1))).expect("the link is inspected");
    assert!(
        link.file_type().is_symlink(),
        "the symbolic link must not be replaced"
    );
    assert_eq!(
        fs::read_to_string(&target).expect("the link target is readable"),
        "untouched",
        "the link target must not be touched"
    );
    assert_eq!(
        fs::read_to_string(dir.join(ACTIVE_LOG)).expect("the active log is readable"),
        "yesterday",
        "the active file must not be rotated away"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// A rollover never replaces a non-regular destination either, and an unsafe
/// target found late makes the whole rotation fail closed: no archive is
/// shifted before the unsafe target is discovered.
#[test]
fn a_rollover_into_a_nonregular_destination_fails_closed() {
    let dir = test_dir("rotate-directory");
    fs::create_dir(dir.join(archive_name(1))).expect("the archive slot is a directory");
    write_file(
        &dir.join(ACTIVE_LOG),
        "yesterday",
        Duration::from_secs(48 * 60 * 60),
    );

    assert!(
        append_line(&dir, b"today\n").is_err(),
        "the rotation must fail closed"
    );
    assert!(
        dir.join(archive_name(1)).is_dir(),
        "the non-regular destination must not be replaced"
    );
    assert_eq!(
        fs::read_to_string(dir.join(ACTIVE_LOG)).expect("the active log is readable"),
        "yesterday",
        "the active file must not be rotated away"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// An unsafe archive found late stops the whole rotation before any archive
/// is shifted.
#[test]
fn a_rollover_fails_closed_before_any_archive_is_shifted() {
    let dir = test_dir("rotate-late");
    write_file(&dir.join(archive_name(1)), "first", Duration::ZERO);
    write_file(&dir.join(archive_name(2)), "second", Duration::ZERO);
    fs::create_dir(dir.join(archive_name(3))).expect("the archive slot is a directory");
    write_file(
        &dir.join(ACTIVE_LOG),
        "yesterday",
        Duration::from_secs(48 * 60 * 60),
    );

    assert!(
        append_line(&dir, b"today\n").is_err(),
        "the rotation must fail closed"
    );
    assert_eq!(
        fs::read_to_string(dir.join(archive_name(1))).expect("the newest archive is readable"),
        "first",
        "no archive may be shifted before the unsafe target is discovered"
    );
    assert_eq!(
        fs::read_to_string(dir.join(archive_name(2))).expect("the second archive is readable"),
        "second",
        "no archive may be shifted before the unsafe target is discovered"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// A nested timing map with an unknown key is rejected as a whole: the event
/// is never echoed, and the valid aggregate timings still round-trip.
#[test]
fn a_nested_unsafe_timing_key_is_rejected_and_never_echoed() {
    let dir = test_dir("nested");
    let logger = DiagLogger::open_in(&dir, "remove", count_warning).expect("the logger opens");
    logger.event(EventKind::RunStart).write();

    let unsafe_line = serde_json::json!({
        "schema_version": 1,
        "run_id": "run-1",
        "seq": 2,
        "ts_ms": 2,
        "elapsed_ms": 0.0,
        "command": "worktree_prune",
        "mode": "remove",
        "kind": "run_end",
        "timings": {
            "total_ms": 1.0,
            "stages": {"/home/user/secret": {"calls": 1, "duration_ms": 1.0}},
            "operations": {}
        }
    })
    .to_string();
    let active = dir.join(ACTIVE_LOG);
    let mut content = fs::read_to_string(&active).expect("the active log is readable");
    content.push_str(&unsafe_line);
    content.push('\n');
    fs::write(&active, content).expect("the active log is written");

    let result = events(&dir, None);
    assert_eq!(
        result.skipped_malformed, 1,
        "the nested unsafe key rejects the whole event"
    );
    let rendered = serde_json::to_string(&result.entries).expect("the entries render");
    assert!(
        !rendered.contains("secret"),
        "no nested key may be echoed: {rendered}"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// The valid aggregate timings still round-trip through the log.
#[test]
fn the_valid_aggregate_timings_round_trip() {
    let dir = test_dir("nested-valid");
    let logger = DiagLogger::open_in(&dir, "remove", count_warning).expect("the logger opens");
    let event = logger
        .event(EventKind::RunEnd)
        .timings(super::event::RunTimings {
            total_ms: 12.5,
            stages: [(
                "scan".to_owned(),
                super::event::RunTimingEntry {
                    calls: 1,
                    duration_ms: 4.0,
                },
            )]
            .into_iter()
            .collect(),
            operations: [(
                "host_list".to_owned(),
                super::event::RunTimingEntry {
                    calls: 2,
                    duration_ms: 1.5,
                },
            )]
            .into_iter()
            .collect(),
        })
        .write();
    assert!(event, "the valid event is written");

    let result = events(&dir, None);
    assert_eq!(
        result.skipped_malformed, 0,
        "the valid event is well formed"
    );
    let timings = result.entries[0]
        .timings
        .as_ref()
        .expect("the timings are kept");
    assert_eq!(
        timings.stages.get("scan").expect("the stage is kept").calls,
        1
    );
    assert_eq!(
        timings
            .operations
            .get("host_list")
            .expect("the operation is kept")
            .calls,
        2
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// A busy lock at startup defers the retention sweep instead of disabling the
/// run: the warning is reported once, the sweep runs with the first append
/// that owns the lock, and the events of the run are written.
#[test]
fn a_busy_lock_at_startup_defers_the_sweep_and_the_run_recovers() {
    let dir = test_dir("startup-busy");
    let aged = dir.join(archive_name(1));
    write_file(&aged, "old", Duration::from_secs(20 * 24 * 60 * 60));
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(LOCK_FILE))
        .expect("the lock file is opened");
    lock.try_lock().expect("the test holds the lock");
    let before = warnings();

    let logger = DiagLogger::open_in(&dir, "remove", count_warning).expect("the logger opens");
    assert_eq!(
        warnings() - before,
        1,
        "the deferred sweep reports one bounded warning"
    );
    assert!(
        aged.exists(),
        "the sweep is deferred while the lock is busy"
    );

    drop(lock);
    assert!(
        logger.event(EventKind::RunStart).write(),
        "the run writes its events after the lock is released"
    );
    assert!(
        !aged.exists(),
        "the deferred sweep runs before the first appended event"
    );
    assert_eq!(
        warnings() - before,
        1,
        "the recovery reports no second warning"
    );
    let result = events(&dir, None);
    assert_eq!(result.entries.len(), 1, "the event was appended");
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

thread_local! {
    /// Whether the injected sweep fails. The flag is per thread, so the
    /// parallel test run never mixes two tests' failures.
    static FAIL_SWEEP: Cell<bool> = const { Cell::new(false) };
}

/// A sweep that fails on demand, so the maintenance-failure path is covered
/// deterministically without a filesystem fault.
fn injected_sweep(dir: &Path) -> Result<(), AppendError> {
    if FAIL_SWEEP.with(Cell::get) {
        return Err(AppendError::Io);
    }
    apply_retention(dir)
}

/// A startup sweep that actually fails stops the run: one bounded warning, no
/// event appended, no later event appended, and no unrelated file touched.
#[test]
fn a_startup_sweep_failure_stops_the_run_without_appending() {
    let dir = test_dir("startup-failure");
    let aged = dir.join(archive_name(1));
    write_file(&aged, "old", Duration::from_secs(20 * 24 * 60 * 60));
    let unrelated = dir.join("keep.txt");
    write_file(&unrelated, "keep", Duration::ZERO);
    FAIL_SWEEP.with(|flag| flag.set(true));
    let before = warnings();

    let logger = DiagLogger::open_in_with(&dir, "remove", count_warning, injected_sweep)
        .expect("the logger opens");
    assert_eq!(
        warnings() - before,
        1,
        "the failed maintenance reports one bounded warning"
    );
    assert!(
        !logger.event(EventKind::RunStart).write(),
        "no event is appended after a failed maintenance"
    );
    assert!(
        !logger.event(EventKind::RunEnd).write(),
        "no later event is appended after a failed maintenance"
    );
    assert_eq!(warnings() - before, 1, "the warning is reported once");
    assert!(
        aged.exists(),
        "the failed sweep leaves the expired archive in place"
    );
    assert!(unrelated.exists(), "no unrelated file is mutated");
    assert!(
        !dir.join(ACTIVE_LOG).exists(),
        "no event reaches the log after a failed maintenance"
    );

    // The next invocation initializes and retries the sweep normally.
    FAIL_SWEEP.with(|flag| flag.set(false));
    let next = DiagLogger::open_in_with(&dir, "remove", count_warning, injected_sweep)
        .expect("the next logger opens");
    assert!(
        next.event(EventKind::RunStart).write(),
        "the next invocation writes its events"
    );
    assert!(
        !aged.exists(),
        "the next invocation completes the deferred maintenance"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// A deferred sweep that actually fails under an acquired lock drops the
/// event and stops the run: nothing is appended while the required
/// maintenance is incomplete.
#[test]
fn a_deferred_sweep_failure_drops_the_event_and_stops_the_run() {
    let dir = test_dir("deferred-failure");
    let aged = dir.join(archive_name(1));
    write_file(&aged, "old", Duration::from_secs(20 * 24 * 60 * 60));
    let unrelated = dir.join("keep.txt");
    write_file(&unrelated, "keep", Duration::ZERO);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(LOCK_FILE))
        .expect("the lock file is opened");
    lock.try_lock().expect("the test holds the lock");
    FAIL_SWEEP.with(|flag| flag.set(true));
    let before = warnings();

    let logger = DiagLogger::open_in_with(&dir, "remove", count_warning, injected_sweep)
        .expect("the logger opens");
    assert_eq!(
        warnings() - before,
        1,
        "the deferred sweep reports one bounded warning"
    );
    assert!(aged.exists(), "the sweep is still deferred");

    drop(lock);
    assert!(
        !logger.event(EventKind::RunStart).write(),
        "the event is not appended after a failed maintenance"
    );
    assert!(
        !logger.event(EventKind::RunEnd).write(),
        "no later event is appended after a failed maintenance"
    );
    assert_eq!(warnings() - before, 1, "the warning is reported once");
    assert!(
        aged.exists(),
        "the failed sweep leaves the expired archive in place"
    );
    assert!(unrelated.exists(), "no unrelated file is mutated");
    assert!(
        !dir.join(ACTIVE_LOG).exists(),
        "no event reaches the log after a failed maintenance"
    );
    assert!(
        events(&dir, None).entries.is_empty(),
        "the log holds no event of the failed run"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}
