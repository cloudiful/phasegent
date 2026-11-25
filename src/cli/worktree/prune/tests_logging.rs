//! Unit tests of the prune diagnostic wiring.
//!
//! The tests work in their own directory under the system temporary
//! directory, so no test writes into the real log directory and no test
//! needs a database, a network, or a process fixture.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use super::timing;
use super::timing::TimingStats;
use crate::infra::diag_log::{DiagLogger, EventKind, LogQuery};

/// A fresh, empty directory for one test.
fn test_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "phasegent-prune-log-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("the test directory is created");
    path
}

/// The measured calls of one stage or operation.
fn calls(timing: &TimingStats, name: &str) -> u64 {
    timing.calls_of(name)
}

/// Without a logger the measurement sink keeps its counts and writes
/// nothing: the unit and default constructors never touch the log.
#[test]
fn the_measurement_counts_are_unchanged_without_a_logger() {
    let dir = test_dir("no-logger");
    let timing = TimingStats::new();
    timing.measure_stage(timing::SCAN, || 7);
    timing.measure_stage(timing::SCAN, || 7);
    timing.measure_operation(timing::HOST_LIST, || ());
    assert_eq!(
        calls(&timing, timing::SCAN),
        2,
        "every stage call is counted"
    );
    assert_eq!(
        calls(&timing, timing::HOST_LIST),
        1,
        "every operation call is counted"
    );
    assert!(
        !dir.join("prune.jsonl").exists(),
        "a measurement without a logger writes nothing"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// With a logger the same measurements also produce typed events, and the
/// events carry the numeric context and the categorical names only.
#[test]
fn the_measured_calls_produce_typed_events_without_any_private_value() {
    let dir = test_dir("events");
    let logger =
        Rc::new(DiagLogger::open_in(&dir, "remove", |_message| {}).expect("the logger opens"));
    let timing = TimingStats::with_logger(Rc::clone(&logger));
    timing.set_repo(1);
    timing.set_candidate(2);
    timing.measure_stage(timing::SCAN, || ());
    timing.candidate_result("pruned", None);

    let result = crate::infra::diag_log::query_in(
        &dir,
        &LogQuery {
            run: None,
            limit: 500,
        },
    );
    assert_eq!(result.skipped_malformed, 0, "every event is well formed");
    assert!(
        result.entries.len() >= 3,
        "the stage start, the stage end, and the candidate result are logged"
    );
    let stage_start = result
        .entries
        .iter()
        .find(|event| event.kind == EventKind::StageStart)
        .expect("the stage start is logged");
    assert_eq!(stage_start.stage.as_deref(), Some(timing::SCAN));
    assert_eq!(
        stage_start.repo,
        Some(1),
        "the repository ordinal is logged"
    );
    assert_eq!(
        stage_start.candidate,
        Some(2),
        "the candidate ordinal is logged"
    );
    let candidate = result
        .entries
        .iter()
        .find(|event| event.kind == EventKind::CandidateResult)
        .expect("the candidate result is logged");
    assert_eq!(candidate.outcome.as_deref(), Some("pruned"));

    // The rendered events carry no path, no identity, and no raw error.
    let rendered = serde_json::to_string(&result.entries).expect("the events render");
    for forbidden in ["/", "path", "error", "session", "issue"] {
        assert!(
            !rendered.contains(forbidden),
            "the events must not carry '{forbidden}': {rendered}"
        );
    }
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// The candidate result carries the safe reason code, and a code outside
/// the fixed vocabulary is never written.
#[test]
fn a_candidate_result_carries_only_the_safe_reason_codes() {
    let dir = test_dir("reasons");
    let logger =
        Rc::new(DiagLogger::open_in(&dir, "dry_run", |_message| {}).expect("the logger opens"));
    let timing = TimingStats::with_logger(Rc::clone(&logger));
    timing.candidate_result("skipped_protected", Some("running_session"));
    timing.candidate_result("ok", Some("a reason outside the vocabulary"));

    let result = crate::infra::diag_log::query_in(
        &dir,
        &LogQuery {
            run: None,
            limit: 500,
        },
    );
    let reasons: Vec<&str> = result
        .entries
        .iter()
        .filter_map(|event| event.reason.as_deref())
        .collect();
    assert_eq!(
        reasons,
        vec!["running_session"],
        "only a known code is written"
    );
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}

/// The retention sweep of the writer never touches an unrelated file, and
/// the aged owned file is removed on the next writer start.
#[test]
fn the_writer_start_sweeps_only_the_owned_aged_files() {
    let dir = test_dir("retention");
    let aged = dir.join("prune.jsonl.1");
    fs::write(&aged, "old").expect("the aged file is written");
    let file = fs::File::open(&aged).expect("the aged file is opened");
    file.set_modified(SystemTime::now() - Duration::from_secs(20 * 24 * 60 * 60))
        .expect("the aged file is stamped");
    let unrelated = dir.join("keep.txt");
    fs::write(&unrelated, "keep").expect("the unrelated file is written");

    let _logger = DiagLogger::open_in(&dir, "remove", |_message| {}).expect("the logger opens");

    assert!(!aged.exists(), "the aged owned file is swept");
    assert!(unrelated.exists(), "an unrelated file is never swept");
    fs::remove_dir_all(&dir).expect("the test directory is removed");
}
