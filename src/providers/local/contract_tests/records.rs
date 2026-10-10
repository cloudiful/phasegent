//! Local-store contract for structured records.
//!
//! A record is an ordinary `local_comments` row whose body opens with the
//! CLI-owned envelope. These tests pin the two local facts the record
//! protocol depends on: the create/get/list paths decode the same
//! structured fields the Redmine path produces, and the row's
//! `role`/`phase`/`attempt` columns are derived from that envelope, while
//! the native reference keeps the historical `#note-<id>` anchor that the
//! Redmine `#change-<id>` change deliberately did not touch.

use super::tmp_provider;
use crate::policy::Role;
use crate::record::{self, RecordFilter, RecordKind, RecordSpec};

fn executor_spec(key: &str) -> RecordSpec {
    RecordSpec {
        kind: RecordKind::Executor,
        actor: Role::Executor,
        key: key.to_owned(),
        phase: Some("P1".to_owned()),
        attempt: Some(2),
        review: None,
        recon: None,
    }
}

fn recon_spec(key: &str) -> RecordSpec {
    RecordSpec {
        kind: RecordKind::Recon,
        actor: Role::Explore,
        key: key.to_owned(),
        phase: None,
        attempt: None,
        review: None,
        recon: Some("scan".to_owned()),
    }
}

fn reviewer_spec(key: &str, review: Option<&str>) -> RecordSpec {
    RecordSpec {
        kind: RecordKind::Reviewer,
        actor: Role::Reviewer,
        key: key.to_owned(),
        phase: Some("final".to_owned()),
        attempt: Some(1),
        review: review.map(str::to_owned),
        recon: None,
    }
}

#[test]
fn create_get_and_list_round_trip_a_local_record() {
    let (provider, dir) = tmp_provider("record-round-trip");
    let issue = provider.create_issue("Issue", "body").unwrap();
    let spec = executor_spec("issue754-P1-a5-records");

    let created = record::create(&provider, issue.number, &spec, "plain note body").unwrap();
    assert_eq!(created.issue, issue.number);
    assert_eq!(created.kind, "executor");
    assert_eq!(created.actor, "executor");
    assert_eq!(created.key, "issue754-P1-a5-records");
    assert_eq!(created.phase.as_deref(), Some("P1"));
    assert_eq!(created.attempt, Some(2));
    // The agent never sees or supplies the generated header.
    assert_eq!(created.body, "plain note body");
    assert!(!created.body.contains("phasegent-record"));
    // Local keeps its own `#note-<id>` anchor.
    assert!(
        created
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with(&format!("#note-{}", created.id))),
        "html_url: {:?}",
        created.html_url
    );

    let fetched = record::get(&provider, issue.number, created.id).unwrap();
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.body, "plain note body");
    assert_eq!(fetched.phase.as_deref(), Some("P1"));

    let listed = record::list(&provider, issue.number, RecordFilter::default()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    let filtered = record::list(
        &provider,
        issue.number,
        RecordFilter {
            kind: Some(RecordKind::Recon),
            phase: None,
            recon: None,
        },
    )
    .unwrap();
    assert!(filtered.is_empty(), "a kind filter must match exactly");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_local_record_row_derives_role_phase_and_attempt_columns() {
    let (provider, dir) = tmp_provider("record-columns");
    let issue = provider.create_issue("Issue", "body").unwrap();

    let executor =
        record::create(&provider, issue.number, &executor_spec("k-exec"), "note").unwrap();
    let recon = record::create(&provider, issue.number, &recon_spec("k-recon"), "scan").unwrap();

    let columns = |id: u64| -> (String, String, i64) {
        provider
            .with_conn("test", |conn| {
                conn.query_row(
                    "SELECT role, phase, attempt FROM local_comments WHERE id=?1",
                    rusqlite::params![id as i64],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
            })
            .unwrap()
    };

    assert_eq!(
        columns(executor.id),
        ("executor".to_owned(), "P1".to_owned(), 2)
    );
    // Recon carries no phase and no attempt: the columns keep the local
    // defaults rather than inventing a phase.
    assert_eq!(columns(recon.id), ("explore".to_owned(), String::new(), 1));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_ordinary_local_comment_is_not_a_record() {
    let (provider, dir) = tmp_provider("record-ordinary");
    let issue = provider.create_issue("Issue", "body").unwrap();
    provider
        .create_comment(issue.number, "<!-- m --> ordinary note", "<!-- m -->")
        .unwrap();
    record::create(&provider, issue.number, &executor_spec("k"), "real").unwrap();

    let listed = record::list(&provider, issue.number, RecordFilter::default()).unwrap();
    assert_eq!(listed.len(), 1, "only the envelope body is a record");
    assert_eq!(listed[0].body, "real");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_local_key_replay_returns_the_existing_record_without_a_second_row() {
    let (provider, dir) = tmp_provider("record-replay");
    let issue = provider.create_issue("Issue", "body").unwrap();
    let spec = executor_spec("k-replay");

    let first = record::create(&provider, issue.number, &spec, "note").unwrap();
    let replay = record::create(&provider, issue.number, &spec, "note").unwrap();
    assert_eq!(replay.id, first.id);

    let all = provider.list_comments(issue.number).unwrap();
    assert_eq!(all.len(), 1, "a replay must not append a second row");

    // A reused key with different content is a conflict, not a second row.
    let conflict = record::create(&provider, issue.number, &spec, "changed").unwrap_err();
    assert!(
        conflict.to_string().contains("already identifies"),
        "{conflict}"
    );
    assert_eq!(provider.list_comments(issue.number).unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_reviewer_record_round_trips_and_a_missing_scope_is_rejected() {
    let (provider, dir) = tmp_provider("record-reviewer-scope");
    let issue = provider.create_issue("Issue", "body").unwrap();

    // A reviewer record without a scope is rejected before any row exists:
    // the encoder refuses an envelope its own decoder would reject.
    let error = record::create(
        &provider,
        issue.number,
        &reviewer_spec("k-noscope", None),
        "verdict",
    )
    .unwrap_err();
    assert!(error.to_string().contains("--review"), "{error}");
    assert_eq!(provider.list_comments(issue.number).unwrap().len(), 0);

    // The explicit scope is stored and decoded verbatim.
    let created = record::create(
        &provider,
        issue.number,
        &reviewer_spec("k-scope", Some("checkpoint")),
        "verdict",
    )
    .unwrap();
    assert_eq!(created.kind, "reviewer");
    assert_eq!(created.actor, "reviewer");
    assert_eq!(created.review.as_deref(), Some("checkpoint"));
    let fetched = record::get(&provider, issue.number, created.id).unwrap();
    assert_eq!(fetched.review.as_deref(), Some("checkpoint"));

    let _ = std::fs::remove_dir_all(dir);
}
