//! Local round-trip and the authorized recon write: create/get/list return
//! the plain body plus structured fields, and explore may publish exactly
//! one authorized recon record.

use super::fixtures::{Scratch, init_local, json_stdout, run_local, seed_issue};
use super::support::{stderr_text, stdout_text};

/// create/get/list round-trip through the local provider, and the agent
/// only ever sees the plain body plus structured fields.
#[test]
fn record_create_get_and_list_round_trip_on_the_local_provider() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "Record host");
    let issue_s = issue.to_string();

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
            "issue754-P1-a5-records",
            "--phase",
            "P1",
            "--attempt",
            "5",
            "--body",
            "plain note body",
            "--authorized",
        ],
    );
    assert_eq!(created.status.code(), Some(0), "{}", stderr_text(&created));
    let record = json_stdout(&created);
    assert_eq!(record["kind"], "executor");
    assert_eq!(record["actor"], "executor");
    assert_eq!(record["key"], "issue754-P1-a5-records");
    assert_eq!(record["phase"], "P1");
    assert_eq!(record["attempt"], 5);
    assert_eq!(record["body"], "plain note body");
    let id = record["id"].as_u64().expect("record id");
    assert!(
        record["html_url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("#note-{id}")),
        "local anchor: {:?}",
        record["html_url"]
    );

    let id_s = id.to_string();
    let fetched = run_local(
        &db,
        "executor",
        &["--provider", "local", "record", "get", &issue_s, &id_s],
    );
    assert_eq!(fetched.status.code(), Some(0), "{}", stderr_text(&fetched));
    let got = json_stdout(&fetched);
    assert_eq!(got["id"], id);
    assert_eq!(got["body"], "plain note body");
    assert_eq!(got["key"], "issue754-P1-a5-records");

    // Explore is one of the record-read roles.
    let listed = run_local(
        &db,
        "explore",
        &["--provider", "local", "record", "list", &issue_s],
    );
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_text(&listed));
    let records: Vec<serde_json::Value> =
        serde_json::from_str(stdout_text(&listed).trim()).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], id);

    let filtered = run_local(
        &db,
        "explore",
        &[
            "--provider",
            "local",
            "record",
            "list",
            &issue_s,
            "--kind",
            "recon",
        ],
    );
    let filtered: Vec<serde_json::Value> =
        serde_json::from_str(stdout_text(&filtered).trim()).unwrap();
    assert!(filtered.is_empty(), "a kind filter matches exactly");
}

/// An authorized recon record is the one write explore may perform.
#[test]
fn explore_publishes_an_authorized_recon_record() {
    let scratch = Scratch::new();
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "Recon host");
    let issue_s = issue.to_string();

    let out = run_local(
        &db,
        "explore",
        &[
            "--provider",
            "local",
            "record",
            "create",
            &issue_s,
            "--kind",
            "recon",
            "--key",
            "recon-1",
            "--recon",
            "scan",
            "--body",
            "findings",
            "--authorized",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let record = json_stdout(&out);
    assert_eq!(record["kind"], "recon");
    assert_eq!(record["actor"], "explore");
    assert_eq!(record["recon"], "scan");
    assert!(record["phase"].is_null());
    assert!(record["attempt"].is_null());
}
