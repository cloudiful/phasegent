//! Error scenarios: a malformed reserved prefix is a reported failure,
//! never silently skipped.

use crate::providers::redmine::contract_tests::support::{
    MockResponse, issue_response, provider, sequence,
};
use crate::record;

/// A journal whose reserved prefix is malformed is an error, never
/// silently skipped: a corrupted record must not read back as ordinary
/// prose.
#[test]
fn a_malformed_reserved_header_is_reported_not_skipped() {
    // Both a terminated but non-envelope payload and an unterminated one
    // error: a reserved prefix at byte 0 is either a complete valid
    // envelope or a reported failure, never silently skipped.
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

        let error = record::list(&redmine, 21, Default::default())
            .expect_err("a malformed envelope must not be ignored");
        server.join().unwrap();
        requests.recv().unwrap();

        assert!(error.to_string().contains("record header"), "{error}");
    }
}
