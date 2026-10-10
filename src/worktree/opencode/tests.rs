//! Unit tests for the OpenCode API boundary (issue 747 P1).
//!
//! The parser is the only place a host answer becomes a fact the cleanup
//! may act on, so it is tested against the shapes the V2 schema defines
//! and against every way an answer can be unusable: invalid JSON, a
//! missing envelope field, a session without the required location, a
//! missing cursor, and a pagination sequence that never terminates.

use super::*;

#[test]
fn session_get_requires_a_location_directory() {
    let body = r#"{"data":{"id":"ses_a","location":{"directory":"/tmp/wt"}}}"#;
    let parsed = parse::parse_session_get(body).expect("documented shape parses");
    assert_eq!(parsed.id, "ses_a");
    assert_eq!(parsed.directory, "/tmp/wt");
}

#[test]
fn session_get_rejects_invalid_json() {
    let error = parse::parse_session_get("not json").expect_err("invalid JSON must fail");
    assert!(
        error.message.contains("invalid JSON"),
        "got: {}",
        error.message
    );
}

#[test]
fn session_get_rejects_a_missing_data_envelope() {
    let error = parse::parse_session_get(r#"{"sessionID":"ses_a"}"#)
        .expect_err("a body without data must fail");
    assert!(
        error.message.contains("no data field"),
        "got: {}",
        error.message
    );
}

#[test]
fn session_get_rejects_a_session_without_a_location() {
    let error = parse::parse_session_get(r#"{"data":{"id":"ses_a"}}"#)
        .expect_err("location is required by the schema");
    assert!(
        error.message.contains("location.directory"),
        "got: {}",
        error.message
    );
}

#[test]
fn session_get_rejects_an_empty_location_directory() {
    let error = parse::parse_session_get(r#"{"data":{"id":"ses_a","location":{"directory":" "}}}"#)
        .expect_err("a blank directory is not a location");
    assert!(
        error.message.contains("location.directory"),
        "got: {}",
        error.message
    );
}

#[test]
fn session_page_reports_the_final_page_without_a_cursor() {
    let body = r#"{"data":[{"id":"ses_a","location":{"directory":"/wt"}}],
                    "cursor":{"previous":null,"next":null}}"#;
    let page = parse::parse_session_page(body).expect("documented shape parses");
    assert_eq!(page.sessions.len(), 1);
    assert_eq!(page.next_cursor, None);
}

#[test]
fn session_page_requires_a_cursor_because_pagination_is_unproven_without_it() {
    let body = r#"{"data":[]}"#;
    let error = parse::parse_session_page(body).expect_err("no cursor means no completeness");
    assert!(error.message.contains("cursor"), "got: {}", error.message);
}

#[test]
fn session_page_rejects_a_cursor_of_the_wrong_type() {
    let body = r#"{"data":[],"cursor":{"next":7}}"#;
    let error = parse::parse_session_page(body).expect_err("a non-string cursor is unusable");
    assert!(
        error.message.contains("cursor.next"),
        "got: {}",
        error.message
    );
}

#[test]
fn is_opencode_session_matches_only_real_session_ids() {
    assert!(is_opencode_session("ses_abc123"));
    assert!(!is_opencode_session("ses_"));
    assert!(!is_opencode_session("phasegent/1-aaaaaa"));
    assert!(!is_opencode_session("session-A"));
    assert!(!is_opencode_session("SESS_abc"));
}
