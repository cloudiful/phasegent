//! Native journal identity: the journal id is the record id and anchors the
//! stable `#change-<id>` history element, and a `204 No Content` write is
//! resolved by re-reading rather than writing again.

use crate::policy::Role;
use crate::providers::redmine::contract_tests::support::{
    MockResponse, issue_response, provider, sequence,
};
use crate::record::RecordKind;

use super::fixtures::{spec, stored};

#[test]
fn a_created_record_carries_the_native_journal_id_and_change_anchor() {
    let spec = spec(
        RecordKind::Executor,
        Role::Executor,
        "issue754-P1-a3-records",
    );
    let body = "plain note body";
    let notes = stored(&spec, body);
    let marker = notes.lines().next().unwrap().to_owned();

    // The PUT returns 204 with no payload, so the provider re-reads the
    // issue to locate the journal it just wrote.
    let (base, requests, server) = sequence(vec![
        MockResponse::status(204, ""),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(501, &notes)])),
    ]);
    let redmine = provider(base);

    let created = redmine
        .create_comment(21, &notes, &marker)
        .expect("the created journal is located after a 204");
    server.join().unwrap();
    let sent = requests.recv().unwrap();

    // The native journal id is the record id.
    assert_eq!(created.id, 501);
    // The reference anchors the stable history element, not the per-page
    // ordinal. Journal 501 is the third journal on the page, so the two
    // differ: this is exactly the unequal ordinal/id case. The host is the
    // per-test mock address, so only the path and anchor are pinned.
    assert!(
        created
            .html_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/issues/21#change-501")),
        "record references must cite the global journal id anchor: {:?}",
        created.html_url
    );
    // The write carried the CLI-generated header and the plain body, and
    // the agent never hand-wrote the header.
    assert!(sent[0].contains("PUT /issues/21.json"), "{sent:?}");
    assert!(sent[0].contains("phasegent-record"), "{sent:?}");
    assert!(sent[0].contains("plain note body"), "{sent:?}");
}

/// A 204 leaves the write result unknown. The service recovers by bounded
/// reads; when the record is found it returns it, and it never issues a
/// second PUT.
#[test]
fn a_lost_write_result_is_recovered_by_reading_and_never_written_again() {
    let spec = spec(RecordKind::Executor, Role::Executor, "k-recover");
    let body = "note";
    let notes = stored(&spec, body);
    let marker = notes.lines().next().unwrap().to_owned();

    // PUT 204, then the recovery listing that finds the journal.
    let (base, requests, server) = sequence(vec![
        MockResponse::status(204, ""),
        MockResponse::ok(issue_response(21, "Title", "Body", false, &[(700, &notes)])),
    ]);
    let redmine = provider(base);

    // `create_comment` is the primitive under test: it must resolve a 204
    // by re-reading instead of reporting an unknown result.
    let output = redmine
        .create_comment(21, &notes, &marker)
        .expect("a 204 is resolved by the provider re-read");
    server.join().unwrap();
    let sent = requests.recv().unwrap();

    assert_eq!(output.id, 700);
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
