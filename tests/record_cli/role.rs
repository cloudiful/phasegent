//! Explore stays read-only outside an authorized recon record: the ordinary
//! comment and issue write surfaces remain denied.

use super::fixtures::{
    Scratch, error_envelope, init_local, json_stdout, run_local, run_local_with_role,
    run_local_with_session, seed_issue,
};
use super::support::{stderr_text, stdout_text};

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

/// Operator never authors an agent record: `record create` is denied with a
/// comment-create hint before any provider access, while `record get`/`list`
/// read through the central admin credential.
#[test]
fn operator_record_create_denied_and_reads_allowed() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "Operator record host");
    let issue_s = issue.to_string();

    // Seed one executor record so Operator reads have something to return.
    let created = run_local(
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
            "operator-read-seed",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "plain note body",
            "--authorized",
        ],
    );
    assert_eq!(created.status.code(), Some(0), "{}", stderr_text(&created));
    let record = json_stdout(&created);
    let id = record["id"].as_u64().expect("record id");
    let id_s = id.to_string();

    // Operator `record create` is denied with the comment hint and no stdout.
    let denied = run_local_with_role(
        &db,
        None,
        &[
            "--provider",
            "local",
            "record",
            "create",
            &issue_s,
            "--kind",
            "executor",
            "--key",
            "operator-forged",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
        ],
    );
    assert_eq!(denied.status.code(), Some(3), "{}", stderr_text(&denied));
    assert_eq!(
        stdout_text(&denied),
        "",
        "a denial prints nothing on stdout"
    );
    let envelope = error_envelope(&denied);
    assert_eq!(envelope["error"]["kind"], "permission");
    assert_eq!(envelope["error"]["operation"], "record create");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("use comment create instead"),
        "operator denial must hint comment create: {}",
        envelope["error"]["message"]
    );

    // Operator reads succeed through the same isolated store.
    let fetched = run_local_with_role(
        &db,
        None,
        &["--provider", "local", "record", "get", &issue_s, &id_s],
    );
    assert_eq!(fetched.status.code(), Some(0), "{}", stderr_text(&fetched));
    assert_eq!(json_stdout(&fetched)["id"], id);

    let listed = run_local_with_role(
        &db,
        None,
        &["--provider", "local", "record", "list", &issue_s],
    );
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_text(&listed));
    let records: Vec<serde_json::Value> =
        serde_json::from_str(stdout_text(&listed).trim()).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], id);
}

/// Operator comments need no child `--authorized` flag.
#[test]
fn operator_comment_create_needs_no_authorized_flag() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "Operator comment host");
    let issue_s = issue.to_string();

    let out = run_local_with_role(
        &db,
        None,
        &[
            "--provider",
            "local",
            "comment",
            "create",
            &issue_s,
            "--marker",
            "op-marker",
            "--body",
            "body contains op-marker",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let value = json_stdout(&out);
    let comment_id = value["id"].as_u64().expect("comment id");
    assert_eq!(
        value["marker"].as_str(),
        Some("op-marker"),
        "create output must carry the marker: {value:?}"
    );
    let comment_id_s = comment_id.to_string();

    // Read-back via `comment get` proves marker/body persistence, not just
    // a non-null envelope.
    let fetched = run_local_with_role(
        &db,
        None,
        &[
            "--provider",
            "local",
            "comment",
            "get",
            &issue_s,
            &comment_id_s,
        ],
    );
    assert_eq!(fetched.status.code(), Some(0), "{}", stderr_text(&fetched));
    let fetched_value = json_stdout(&fetched);
    assert_eq!(fetched_value["id"].as_u64(), Some(comment_id));
    assert_eq!(
        fetched_value["marker"].as_str(),
        Some("op-marker"),
        "read-back marker must match: {fetched_value:?}"
    );
    assert!(
        fetched_value["body"]
            .as_str()
            .is_some_and(|body| body.contains("op-marker")),
        "read-back body must contain the marker: {fetched_value:?}"
    );

    // Bulk read-back via `comment list` carries the same marker/body.
    let listed = run_local_with_role(
        &db,
        None,
        &["--provider", "local", "comment", "list", &issue_s],
    );
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_text(&listed));
    let listed_value = json_stdout(&listed);
    let comments = listed_value["comments"].as_array().expect("comments array");
    assert_eq!(comments.len(), 1, "expected one comment: {listed_value:?}");
    assert_eq!(comments[0]["marker"].as_str(), Some("op-marker"));
    assert!(
        comments[0]["body"]
            .as_str()
            .is_some_and(|body| body.contains("op-marker")),
        "listed body must contain the marker: {listed_value:?}"
    );
}

/// A managed session without a role fails closed for record commands
/// before any provider access, for both present and blank markers.
#[test]
fn managed_without_role_fails_record_commands_before_provider() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    // Presence (even blank) selects managed-missing fail-closed at the
    // parser layer (exit 2, argument envelope) before any provider access.
    for session in ["managed-session", ""] {
        let out = run_local_with_session(
            &db,
            None,
            Some(session),
            &["--provider", "local", "record", "list", "1"],
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "session {session:?}: {}",
            stderr_text(&out)
        );
        assert_eq!(stdout_text(&out), "", "a denial prints nothing on stdout");
        let envelope = error_envelope(&out);
        assert_eq!(envelope["error"]["kind"], "argument");
        assert!(
            envelope["error"]["message"]
                .as_str()
                .unwrap()
                .contains("managed session requires a role"),
            "managed record list must fail closed: {}",
            envelope["error"]["message"]
        );
    }
}
