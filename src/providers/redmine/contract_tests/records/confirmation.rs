//! Successful-write confirmation: the provider's own write response is never
//! authority, so every write is confirmed by a bounded exact decoded
//! owned-key read — a duplicate, header-quoting prose, or a changed record
//! all fail rather than synthesizing a success.

use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, issue_response, provider, sequence,
};
use crate::record::{self, RecordKind};

use super::fixtures::{spec, stored};

/// A primitive success is never its own confirmation: after the transport
/// reports the write succeeded, the service still requires an exact
/// decoded owned-key read, and adopts the id only from the decoded record.
#[test]
fn a_primitive_success_is_confirmed_by_an_exact_decoded_read() {
    let spec = spec(RecordKind::Executor, Role::Executor, "k-confirm");
    let body = "note";
    let notes = stored(&spec, body);

    // Empty preflight, PUT 204, the primitive's recovery read, then the
    // confirmation read: only the decoded journal is authority.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[])),
        MockResponse::status(204, ""),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(700, &notes)])),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(700, &notes)])),
    ]);
    let redmine = provider(base);

    let output = record::create(&redmine, 21, &spec, body).expect("confirmed by an exact read");
    server.join().unwrap();
    let sent = requests.recv().unwrap();

    assert_eq!(output.id, 700);
    assert_eq!(output.body, body);
    assert!(
        output
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/issues/21#change-700")),
        "html_url: {:?}",
        output.html_url
    );
    let puts = sent.iter().filter(|line| line.contains("PUT")).count();
    assert_eq!(puts, 1, "exactly one write attempt: {sent:?}");
}

/// After a successful transport write, two decoded journals claiming the
/// key are a duplicate error, never the last substring match.
#[test]
fn a_successful_write_with_duplicate_journals_is_an_error() {
    let spec = spec(RecordKind::Executor, Role::Executor, "k-success-dupe");
    let body = "note";
    let notes = stored(&spec, body);

    let (base, requests, server) = sequence(vec![
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[])),
        MockResponse::status(204, ""),
        MockResponse::ok(issue_response(
            21,
            "Title",
            "Body",
            false,
            &[(501, &notes), (502, &notes)],
        )),
        MockResponse::ok(issue_response(
            21,
            "Title",
            "Body",
            false,
            &[(501, &notes), (502, &notes)],
        )),
    ]);
    let redmine = provider(base);

    let error = record::create(&redmine, 21, &spec, body)
        .expect_err("a successful transport write is not confirmation of a duplicate");
    server.join().unwrap();
    let sent = requests.recv().unwrap();

    assert!(error.to_string().contains("matches 2 records"), "{error}");
    let puts = sent.iter().filter(|line| line.contains("PUT")).count();
    assert_eq!(puts, 1, "exactly one write attempt: {sent:?}");
}

/// After a successful transport write, an ordinary journal that merely
/// quotes the reserved header in prose is not a record: the write stays
/// unconfirmed and is an error, never a synthesized success.
#[test]
fn a_successful_write_with_only_header_quoting_prose_is_unconfirmed() {
    let spec = spec(RecordKind::Executor, Role::Executor, "k-success-noise");
    let body = "note";
    let notes = stored(&spec, body);
    let marker = notes.lines().next().unwrap().to_owned();
    let prose = format!("prose that mentions {marker} in passing");

    // Empty preflight, PUT 204, then the primitive's substring match and
    // every bounded confirmation read see only the ordinary prose.
    let (base, requests, server) = sequence(vec![
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[])),
        MockResponse::status(204, ""),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(503, &prose)])),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(503, &prose)])),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(503, &prose)])),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(503, &prose)])),
    ]);
    let redmine = provider(base);

    let error = record::create(&redmine, 21, &spec, body)
        .expect_err("a substring match is not a confirmed record");
    server.join().unwrap();
    let sent = requests.recv().unwrap();

    assert!(error.to_string().contains("not confirmed"), "{error}");
    let puts = sent.iter().filter(|line| line.contains("PUT")).count();
    assert_eq!(puts, 1, "exactly one write attempt: {sent:?}");
}

/// After a successful transport write, an owned key whose stored record
/// differs from the request — body or metadata — is a conflict, never a
/// synthesized success.
#[test]
fn a_successful_write_whose_key_now_holds_a_different_record_is_a_conflict() {
    let spec = spec(RecordKind::Executor, Role::Executor, "k-success-conflict");
    let body = "note";
    // Same metadata (so the same header line and marker) but a different
    // stored body, and separately a different actor: the primitive's
    // substring match succeeds for both, the exact request comparison
    // does not.
    let changed_body = stored(&spec, "a different note");
    let mut other_actor = spec.clone();
    other_actor.actor = Role::Orchestrator;
    let changed_actor = stored(&other_actor, body);

    for (label, changed) in [("body", &changed_body), ("actor", &changed_actor)] {
        let (base, requests, server) = sequence(vec![
            MockResponse::ok(issue_response(21, "Title", "Body", false, &[])),
            MockResponse::status(204, ""),
            MockResponse::ok(issue_response(
                21,
                "Title",
                "Body",
                false,
                &[(501, changed)],
            )),
            MockResponse::ok(issue_response(
                21,
                "Title",
                "Body",
                false,
                &[(501, changed)],
            )),
        ]);
        let redmine = provider(base);

        let error = record::create(&redmine, 21, &spec, body).unwrap_err();
        server.join().unwrap();
        let sent = requests.recv().unwrap();

        assert!(
            error
                .to_string()
                .contains("already identifies a different record"),
            "changed {label}: {error}"
        );
        let puts = sent.iter().filter(|line| line.contains("PUT")).count();
        assert_eq!(
            puts, 1,
            "changed {label}: exactly one write attempt: {sent:?}"
        );
    }
}
