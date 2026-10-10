//! Explore stays read-only outside an authorized recon record: the ordinary
//! comment and issue write surfaces remain denied.

use super::fixtures::{Scratch, error_envelope, run_local};
use super::support::stderr_text;

/// Explore stays read-only outside an authorized recon record: the
/// ordinary comment and issue write surfaces remain denied.
#[test]
fn explore_is_read_only_outside_authorized_recon_records() {
    let scratch = Scratch::new();
    let db = scratch.local_db();

    let comment = run_local(
        &db,
        "explore",
        &[
            "--provider",
            "local",
            "comment",
            "create",
            "1",
            "--marker",
            "m",
            "--body",
            "x",
            "--authorized",
        ],
    );
    assert_eq!(comment.status.code(), Some(3), "{}", stderr_text(&comment));
    let envelope = error_envelope(&comment);
    assert_eq!(envelope["error"]["kind"], "permission");
    assert_eq!(envelope["error"]["operation"], "comment create");

    let issue = run_local(
        &db,
        "explore",
        &["issue", "create", "--title", "T", "--body", "B"],
    );
    assert_eq!(issue.status.code(), Some(3), "{}", stderr_text(&issue));
    assert_eq!(error_envelope(&issue)["error"]["kind"], "permission");
}
