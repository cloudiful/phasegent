//! Error scenarios: a malformed reserved prefix is reported as an invalid
//! entry, never silently skipped and never read as a record.

use crate::providers::redmine::contract_tests::support::{
    MockResponse, issue_response, provider, sequence,
};
use crate::record;

/// A journal whose reserved prefix is malformed is listed as an invalid
/// entry: a corrupted record must not read back as ordinary prose, and it
/// must not hide the other records on the issue either.
#[test]
fn a_malformed_reserved_header_is_listed_as_an_invalid_entry() {
    // Both a terminated but non-envelope payload and an unterminated one
    // are invalid entries: a reserved prefix at byte 0 is either a
    // complete valid envelope or an explicitly invalid entry, never
    // silently skipped.
    for malformed in [
        "<!-- phasegent-record not-json -->",
        "<!-- phasegent-record {\"v\":1}",
    ] {
        let (base, requests, server) = sequence(vec![MockResponse::ok(issue_response(
            21,
            "Title",
            "Body",
            false,
            &[(501, malformed)],
        ))]);
        let redmine = provider(base);

        let listed = record::list(&redmine, 21, Default::default())
            .expect("a malformed envelope must not fail the whole listing");
        server.join().unwrap();
        requests.recv().unwrap();

        assert_eq!(listed.len(), 1, "{malformed}");
        let entry = listed[0].record();
        assert!(entry.is_none(), "{malformed} must not read as a record");
    }
}
