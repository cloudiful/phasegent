//! Key replay and conflict: an identical replay returns the existing record
//! without writing, a reused key with different content or two journals
//! claiming one key error instead of guessing.

use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, issue_response, provider, sequence,
};
use crate::record::{self, RecordKind};

use super::fixtures::{spec, stored};

/// A key replay of an identical request returns the same record without a
/// second write; only the pre-write listing runs.
#[test]
fn an_identical_key_replay_returns_the_existing_record_without_writing() {
    let spec = spec(RecordKind::Executor, Role::Executor, "k-replay");
    let body = "note";
    let notes = stored(&spec, body);

    // One listing (inside record::create) and no PUT.
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        21,
        "Title",
        "Body",
        false,
        &[(501, &notes)],
    ))]);
    let redmine = provider(base);

    let output = record::create(&redmine, 21, &spec, body).expect("replay returns the record");
    server.join().unwrap();
    let sent = requests.recv().unwrap();

    assert_eq!(output.id, 501);
    assert_eq!(output.key, "k-replay");
    assert_eq!(output.phase.as_deref(), Some("P1"));
    assert_eq!(output.attempt, Some(2));
    assert_eq!(output.body, body);
    assert_eq!(output.issue, 21);
    assert_eq!(sent.len(), 1, "a replay must not write again: {sent:?}");
    assert!(sent[0].contains("GET /issues/21.json"), "{sent:?}");
}

/// A reused key with different content is a conflict, never a second
/// write.
#[test]
fn a_key_reused_for_different_content_is_a_conflict() {
    let spec = spec(RecordKind::Executor, Role::Executor, "k-conflict");
    let notes = stored(&spec, "original note");
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        21,
        "Title",
        "Body",
        false,
        &[(501, &notes)],
    ))]);
    let redmine = provider(base);

    let error = record::create(&redmine, 21, &spec, "different note")
        .expect_err("a changed body under a used key is a conflict");
    server.join().unwrap();
    let sent = requests.recv().unwrap();

    assert!(
        error
            .to_string()
            .contains("already identifies a different record"),
        "{error}"
    );
    assert_eq!(sent.len(), 1, "a conflict must not write: {sent:?}");
}

/// Two journals claiming one key are reported as duplicates rather than
/// resolved by guessing which one the caller meant.
#[test]
fn two_journals_claiming_one_key_are_an_error_not_a_guess() {
    let spec = spec(RecordKind::Executor, Role::Executor, "k-dupe");
    let notes = stored(&spec, "note");
    let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
        21,
        "Title",
        "Body",
        false,
        &[(501, &notes), (502, &notes)],
    ))]);
    let redmine = provider(base);

    let error = record::create(&redmine, 21, &spec, "note").expect_err("duplicate keys must error");
    server.join().unwrap();
    let sent = requests.recv().unwrap();

    assert!(error.to_string().contains("matches 2 records"), "{error}");
    assert_eq!(sent.len(), 1, "a duplicate must not write: {sent:?}");
}
