//! Read paths: `get` and `list` return only decoded envelopes, ignore
//! ordinary comments, and filter locally and exactly.

use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, issue_response, provider, sequence,
};
use crate::record::{self, RecordKind};

use super::fixtures::{spec, stored};

/// `get` returns the plain body plus the structured fields decoded from
/// the CLI-owned envelope, and the header is never surfaced.
#[test]
fn get_returns_plain_body_and_structured_fields() {
    let spec = spec(RecordKind::Reviewer, Role::Reviewer, "k-get");
    let notes = stored(&spec, "reviewer verdict text");
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        21,
        "Title",
        "Body",
        false,
        &[(501, &notes)],
    ))]);
    let redmine = provider(base);

    let output = record::get(&redmine, 21, 501).expect("the record is readable by native id");
    server.join().unwrap();
    requests.recv().unwrap();

    assert_eq!(output.id, 501);
    assert_eq!(output.issue, 21);
    assert_eq!(output.kind, "reviewer");
    assert_eq!(output.actor, "reviewer");
    assert_eq!(output.key, "k-get");
    assert_eq!(output.phase.as_deref(), Some("P1"));
    assert_eq!(output.attempt, Some(2));
    assert_eq!(output.body, "reviewer verdict text");
    assert!(!output.body.contains("phasegent-record"));
    assert!(
        output
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/issues/21#change-501")),
        "html_url: {:?}",
        output.html_url
    );
}

/// An ordinary comment id is not a record: `get` reports not-found rather
/// than inventing structured fields.
#[test]
fn getting_an_ordinary_comment_is_not_found() {
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        21,
        "Title",
        "Body",
        false,
        &[(501, "just an ordinary note")],
    ))]);
    let redmine = provider(base);

    let error = record::get(&redmine, 21, 501).expect_err("an ordinary comment is not a record");
    server.join().unwrap();
    requests.recv().unwrap();

    assert!(error.to_string().contains("not found"), "{error}");
}

/// An ordinary journal that merely mentions record-looking text is not a
/// record: it is ignored by list and never reported as structured fields.
#[test]
fn an_ordinary_journal_is_not_decoded_as_a_record() {
    let spec = spec(RecordKind::Executor, Role::Executor, "k1");
    let notes = stored(&spec, "real record");
    // An ordinary journal that merely mentions the protocol in prose is
    // not a record: only a body whose byte 0 opens the reserved envelope
    // is ever decoded, so a substring match cannot promote it.
    let ordinary = "prose that mentions <!-- phasegent-record --> in passing";
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        21,
        "Title",
        "Body",
        false,
        &[(501, ordinary), (502, &notes)],
    ))]);
    let redmine = provider(base);

    let records = record::list(&redmine, 21, Default::default()).expect("list succeeds");
    server.join().unwrap();
    requests.recv().unwrap();

    assert_eq!(records.len(), 1, "only the real envelope is a record");
    assert_eq!(records[0].id, 502);
    assert_eq!(records[0].body, "real record");
}

/// `list` filters locally and exactly over the decoded records.
#[test]
fn list_filters_locally_and_exactly() {
    let executor = spec(RecordKind::Executor, Role::Executor, "k1");
    let recon = spec(RecordKind::Recon, Role::Explore, "k2");
    let executor_notes = stored(&executor, "executor note");
    let recon_notes = stored(&recon, "recon note");
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        21,
        "Title",
        "Body",
        false,
        &[(501, &executor_notes), (502, &recon_notes)],
    ))]);
    let redmine = provider(base);

    let filtered = record::list(
        &redmine,
        21,
        crate::record::RecordFilter {
            kind: Some(RecordKind::Recon),
            phase: None,
            recon: Some("scan".to_owned()),
        },
    )
    .expect("list succeeds");
    server.join().unwrap();
    requests.recv().unwrap();

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, 502);
    assert_eq!(filtered[0].kind, "recon");
    assert_eq!(filtered[0].recon.as_deref(), Some("scan"));
}
