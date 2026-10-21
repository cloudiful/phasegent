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
fn session_page_keeps_a_representative_v211_shape_and_an_opaque_cursor() {
    let body = r#"{"data":[{"id":"ses_synthetic01","parentID":"ses_synthetic02",
                    "projectID":"project_synthetic","agent":"executor",
                    "model":{"id":"executor","providerID":"synthetic","variant":"default"},
                    "cost":0,"tokens":{"input":10,"output":2,"reasoning":0,
                    "cache":{"read":0,"write":0}},
                    "time":{"created":1700000000000,"updated":1700000001000},
                    "title":"Synthetic session","permissions":[],
                    "location":{"directory":"/tmp/synthetic/wt"}}],
                    "cursor":{"previous":"cHJldmlvdXM=","next":"bmV4dC1jdXJzb3I="}}"#;
    let page = parse::parse_session_page(body).expect("documented shape parses");
    assert_eq!(page.sessions.len(), 1);
    assert_eq!(page.sessions[0].id, "ses_synthetic01");
    assert_eq!(page.sessions[0].directory, "/tmp/synthetic/wt");
    assert_eq!(page.next_cursor.as_deref(), Some("bmV4dC1jdXJzb3I="));
}

#[test]
fn session_get_accepts_a_representative_v211_session_envelope() {
    let body = r#"{"data":{"id":"ses_synthetic03","parentID":null,
                    "projectID":"project_synthetic","agent":"reviewer",
                    "model":{"id":"reviewer","providerID":"synthetic","variant":"default"},
                    "cost":0,"tokens":{"input":20,"output":4,"reasoning":1,
                    "cache":{"read":0,"write":0}},
                    "time":{"created":1700000000000,"updated":1700000002000},
                    "title":"Synthetic session","permissions":[],
                    "location":{"directory":"/tmp/synthetic/wt"}}}"#;
    let parsed = parse::parse_session_get(body).expect("documented shape parses");
    assert_eq!(parsed.id, "ses_synthetic03");
    assert_eq!(parsed.directory, "/tmp/synthetic/wt");
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

#[test]
fn active_sessions_parses_a_running_map() {
    let body = r#"{"data":{"ses_b":{"type":"running"},"ses_a":{"type":"running"}}}"#;
    let ids = parse::parse_active_sessions(body).expect("documented shape parses");
    assert_eq!(ids, vec!["ses_a".to_owned(), "ses_b".to_owned()]);
}

#[test]
fn active_sessions_accepts_an_empty_map_as_nothing_running() {
    let ids = parse::parse_active_sessions(r#"{"data":{}}"#).expect("empty is valid");
    assert!(ids.is_empty());
}

#[test]
fn active_sessions_rejects_invalid_json() {
    let error =
        parse::parse_active_sessions("not json").expect_err("invalid JSON must fail closed");
    assert!(
        error.message.contains("invalid JSON"),
        "got: {}",
        error.message
    );
}

#[test]
fn active_sessions_rejects_a_missing_data_envelope() {
    let error =
        parse::parse_active_sessions(r#"{"sessions":{}}"#).expect_err("no data must fail closed");
    assert!(
        error.message.contains("no data field"),
        "got: {}",
        error.message
    );
}

#[test]
fn active_sessions_rejects_a_non_object_data() {
    let error = parse::parse_active_sessions(r#"{"data":[]}"#)
        .expect_err("a non-object data must fail closed");
    assert!(
        error.message.contains("data is not an object"),
        "got: {}",
        error.message
    );
}

#[test]
fn active_sessions_rejects_an_empty_session_id() {
    let error = parse::parse_active_sessions(r#"{"data":{"":{"type":"running"}}}"#)
        .expect_err("an empty id must fail closed");
    assert!(
        error.message.contains("empty session id"),
        "got: {}",
        error.message
    );
}

#[test]
fn active_sessions_rejects_a_missing_type() {
    let error = parse::parse_active_sessions(r#"{"data":{"ses_a":{}}}"#)
        .expect_err("a missing type must fail closed");
    assert!(error.message.contains("no type"), "got: {}", error.message);
}

#[test]
fn active_sessions_rejects_an_unknown_type() {
    let error = parse::parse_active_sessions(r#"{"data":{"ses_a":{"type":"background"}}}"#)
        .expect_err("an unknown type must fail closed");
    assert!(
        error.message.contains("unknown type"),
        "got: {}",
        error.message
    );
}
