//! The one-shot `--body-file` lifecycle: validated before provider access,
//! consumed only by a confirmed write, and kept on any failure.

use std::fs;

use super::fixtures::{Scratch, init_local, run_local, seed_issue};
use super::support::stderr_text;

/// The one-shot `--body-file` follows the comment flow: validated before
/// provider access, consumed only by a confirmed write, and kept on any
/// failure or under `--keep-body-file`.
#[test]
fn body_file_is_consumed_only_after_a_confirmed_write() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "Body file host");
    let issue_s = issue.to_string();

    let body_path = scratch.path().join("note.txt");
    fs::write(&body_path, "from a file").unwrap();
    let body_s = body_path.to_str().unwrap().to_owned();
    let ok = run_local(
        &db,
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            &issue_s,
            "--kind",
            "executor",
            "--key",
            "k-file",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body-file",
            &body_s,
            "--authorized",
        ],
    );
    assert_eq!(ok.status.code(), Some(0), "{}", stderr_text(&ok));
    assert!(
        !body_path.exists(),
        "a confirmed write consumes the one-shot body file"
    );

    // A denied request never reads the file.
    let denied_path = scratch.path().join("denied.txt");
    fs::write(&denied_path, "kept").unwrap();
    let denied_s = denied_path.to_str().unwrap().to_owned();
    let denied = run_local(
        &db,
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            &issue_s,
            "--kind",
            "reviewer",
            "--key",
            "k-denied",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--review",
            "final",
            "--body-file",
            &denied_s,
            "--authorized",
        ],
    );
    assert_eq!(denied.status.code(), Some(3));
    assert!(
        denied_path.exists(),
        "a denied request must not consume the body file"
    );

    // `--keep-body-file` preserves it after a successful write.
    let kept_path = scratch.path().join("kept.txt");
    fs::write(&kept_path, "kept").unwrap();
    let kept_s = kept_path.to_str().unwrap().to_owned();
    let kept = run_local(
        &db,
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            &issue_s,
            "--kind",
            "executor",
            "--key",
            "k-kept",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body-file",
            &kept_s,
            "--keep-body-file",
            "--authorized",
        ],
    );
    assert_eq!(kept.status.code(), Some(0), "{}", stderr_text(&kept));
    assert!(
        kept_path.exists(),
        "--keep-body-file must preserve the file"
    );
}
