//! Unconfirmed-create scenarios: a missing reviewer scope is rejected
//! before the body file, and a duplicate key retains the one-shot body
//! file because the write was never confirmed.

use std::fs;

use super::fixtures::{Scratch, error_envelope, init_local, run_local, seed_issue};
use super::support::{stderr_text, stdout_text};

/// A reviewer record without a declared scope fails on the scope gate
/// before the `--body-file` is read: pointing at a missing path proves the
/// ordering, because a file read would have reported a different error.
#[test]
fn a_reviewer_record_without_scope_is_rejected_before_the_body_file() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "Scope host");
    let issue_s = issue.to_string();

    let missing = scratch.path().join("never-read.txt");
    let missing_s = missing.to_str().unwrap().to_owned();
    let out = run_local(
        &db,
        "reviewer",
        &[
            "--provider",
            "local",
            "record",
            "create",
            &issue_s,
            "--kind",
            "reviewer",
            "--key",
            "k-noscope",
            "--phase",
            "final",
            "--attempt",
            "1",
            "--body-file",
            &missing_s,
            "--authorized",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "a missing scope is an argument error: {}",
        stderr_text(&out)
    );
    let envelope = error_envelope(&out);
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--review"),
        "{envelope}"
    );
    assert!(
        !missing.exists(),
        "the scope gate fired before the body file was touched"
    );
}

/// Duplicate owned journals are an error rather than a last-match guess,
/// and the one-shot `--body-file` is retained because the write was never
/// confirmed.
#[test]
fn duplicate_records_error_and_retain_the_body_file() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "Duplicate host");

    // Two stored envelopes claim the same key with different metadata, so
    // both rows decode to the key but the unique marker still admits both.
    let conn = rusqlite::Connection::open(&db).expect("open local db");
    for attempt in [1, 2] {
        let header = format!(
            r#"<!-- phasegent-record {{"v":1,"kind":"executor","actor":"executor","key":"dup-key","phase":"P1","attempt":{attempt}}} -->"#
        );
        let body = format!("{header}\nnote {attempt}");
        conn.execute(
            "INSERT INTO local_comments (issue_id, role, phase, attempt, marker, body, created_at) \
             VALUES (?1, 'executor', 'P1', ?2, ?3, ?4, 1000)",
            rusqlite::params![issue as i64, attempt as i64, header, body],
        )
        .expect("seed duplicate record");
    }
    drop(conn);

    let path = scratch.path().join("retained.txt");
    fs::write(&path, "must be kept").unwrap();
    let path_s = path.to_str().unwrap().to_owned();
    let out = run_local(
        &db,
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            &issue.to_string(),
            "--kind",
            "executor",
            "--key",
            "dup-key",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body-file",
            &path_s,
            "--authorized",
        ],
    );
    assert!(
        !out.status.success(),
        "a duplicate key must fail: {}",
        stdout_text(&out)
    );
    assert!(
        error_envelope(&out)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("matches 2 records"),
        "{}",
        stderr_text(&out)
    );
    assert!(
        path.exists(),
        "an unconfirmed create must retain the one-shot body file"
    );
}
