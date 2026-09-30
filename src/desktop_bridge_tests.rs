//! Unit tests for the hidden desktop bridge: framing, method allowlist,
//! typed parameters, redaction, correlation, and shutdown.
//!
//! Every test runs in memory (an in-memory reader plus a recording writer),
//! so no process, network, or operator database is touched.

use super::*;
use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};

use serde_json::json;

/// Writer that records every byte so a session can be asserted afterwards.
#[derive(Clone, Default)]
struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("recording writer")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run_session(input: &[u8], workers: usize) -> (i32, Vec<Value>) {
    let writer = RecordingWriter::default();
    let code = serve(Cursor::new(input.to_vec()), writer.clone(), workers);
    let recorded = writer.0.lock().expect("recorded output").clone();
    let responses = recorded
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).expect("each response line is JSON"))
        .collect();
    (code, responses)
}

fn request_line(value: Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(&value).expect("request serializes");
    line.push(b'\n');
    line
}

fn message(response: &Value) -> &str {
    response["error"]["message"]
        .as_str()
        .expect("error message is a string")
}

#[test]
fn method_allowlist_is_exact_and_versioned() {
    assert_eq!(PROTOCOL_VERSION, 1);
    let names: Vec<&str> = METHODS.iter().map(|(_, wire)| *wire).collect();
    assert_eq!(
        names,
        [
            "get_app_metadata",
            "get_config_snapshot",
            "get_branch_context",
            "get_tasks",
            "get_status",
            "set_config_setting",
            "clear_config_setting",
            "set_credential",
            "clear_credential",
            "get_provisioning_status",
        ]
    );
    for (method, wire) in METHODS {
        assert_eq!(Method::parse(wire), Some(*method));
        assert_eq!(method.name(), *wire);
    }
    // No CLI group, alias, or whitespace variant is reachable.
    for rejected in [
        "",
        " ",
        "get_tasks ",
        "get_issue",
        "issue",
        "issue get",
        "comment",
        "auth",
        "config",
        "admin",
        "mcp",
        "gui",
        "doctor",
        "notify",
        "worktree",
        "desktop-bridge",
        "get_provisioning_statuses",
    ] {
        assert!(
            Method::parse(rejected).is_none(),
            "{rejected} must be rejected"
        );
    }
}

#[test]
fn unknown_method_is_rejected_with_the_allowlist() {
    let response = handle_line(br#"{"id":7,"method":"issue get","params":{}}"#);
    assert_eq!(response["id"], 7);
    assert_eq!(response["protocol"], PROTOCOL_VERSION);
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["kind"], "method");
    assert!(message(&response).contains("unknown method 'issue get'"));
    assert!(message(&response).contains("get_tasks"));
    assert!(message(&response).chars().count() <= 300);
}

#[test]
fn malformed_input_is_rejected_and_the_stream_recovers() {
    let mut input = Vec::new();
    input.extend_from_slice(b"{not json}\n");
    input.extend_from_slice(b"[1,2]\n");
    input.extend_from_slice(b"\"text\"\n");
    input.extend_from_slice(&[0xff, 0xfe, b'\n']);
    input.extend_from_slice(&request_line(
        json!({"id": 1, "method": "get_app_metadata"}),
    ));
    let (code, responses) = run_session(&input, 1);
    assert_eq!(code, 0);
    assert_eq!(responses.len(), 5);
    for rejected in &responses[..4] {
        assert_eq!(rejected["ok"], false);
        assert_eq!(rejected["error"]["kind"], "parse");
        assert_eq!(rejected["id"], Value::Null);
        assert_eq!(rejected["protocol"], PROTOCOL_VERSION);
    }
    assert_eq!(responses[4]["ok"], true);
    assert_eq!(responses[4]["id"], 1);
}

#[test]
fn oversized_request_is_rejected_without_buffering_it() {
    let mut input = vec![b'x'; MAX_LINE_BYTES + 1];
    input.push(b'\n');
    input.extend_from_slice(&request_line(
        json!({"id": 2, "method": "get_app_metadata"}),
    ));
    let (code, responses) = run_session(&input, 1);
    assert_eq!(code, 0);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["ok"], false);
    assert_eq!(responses[0]["error"]["kind"], "too_large");
    assert!(message(&responses[0]).contains(&MAX_LINE_BYTES.to_string()));
    assert_eq!(responses[1]["id"], 2);
    assert_eq!(responses[1]["ok"], true);
}

#[test]
fn envelope_validation_requires_an_object_with_a_method() {
    let no_method = handle_line(br#"{"id":1}"#);
    assert_eq!(no_method["error"]["kind"], "method");
    assert_eq!(no_method["id"], 1);
    let non_string = handle_line(br#"{"id":1,"method":5}"#);
    assert_eq!(non_string["error"]["kind"], "method");
    let scalar = handle_line(b"3");
    assert_eq!(scalar["error"]["kind"], "parse");
    assert_eq!(scalar["id"], Value::Null);
    // A missing id still gets an answer, correlated as null.
    let no_id = handle_line(br#"{"method":"get_app_metadata"}"#);
    assert_eq!(no_id["id"], Value::Null);
    assert_eq!(no_id["ok"], true);
}

#[test]
fn zero_argument_methods_reject_parameters() {
    let accepted: [&[u8]; 3] = [
        br#"{"id":1,"method":"get_app_metadata"}"#,
        br#"{"id":1,"method":"get_app_metadata","params":null}"#,
        br#"{"id":1,"method":"get_app_metadata","params":{}}"#,
    ];
    for line in accepted {
        assert_eq!(handle_line(line)["ok"], true);
    }
    let rejected: [&[u8]; 3] = [
        br#"{"id":1,"method":"get_app_metadata","params":{"request":{}}}"#,
        br#"{"id":1,"method":"get_app_metadata","params":7}"#,
        br#"{"id":1,"method":"get_app_metadata","params":[]}"#,
    ];
    for line in rejected {
        let response = handle_line(line);
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["kind"], "argument");
        assert!(message(&response).contains("takes no parameters"));
    }
}

#[test]
fn typed_parameters_are_validated_without_echoing_secrets() {
    let missing = handle_line(
        br#"{"id":1,"method":"set_credential","params":{"request":{"role":"executor","provider":"redmine"}}}"#,
    );
    assert_eq!(missing["error"]["kind"], "argument");
    assert!(message(&missing).contains("credential"));
    assert!(message(&missing).contains("set_credential"));

    let scalar_params = handle_line(br#"{"id":2,"method":"get_tasks","params":"open"}"#);
    assert_eq!(scalar_params["error"]["kind"], "argument");
    assert!(message(&scalar_params).contains("params must be a JSON object"));

    // A rejected provider fails before storage, and the credential value
    // never reaches the response.
    let secret = "hunter2-super-secret";
    let credential = handle_line(
        &serde_json::to_vec(&json!({
            "id": 3,
            "method": "set_credential",
            "params": {"request": {"role": "executor", "provider": "bogus", "credential": secret}},
        }))
        .expect("request serializes"),
    );
    assert_eq!(credential["error"]["kind"], "backend");
    let encoded = serde_json::to_string(&credential).expect("response serializes");
    assert!(!encoded.contains(secret));
    assert!(!encoded.contains("hunter2"));

    // Secret settings stay on the credential path and never echo the value.
    let value = "postgres://user:pw@db.example/phasegent";
    let setting = handle_line(
        &serde_json::to_vec(&json!({
            "id": 4,
            "method": "set_config_setting",
            "params": {"request": {"setting": "PHASEGENT_INDEX_PG_URL", "value": value}},
        }))
        .expect("request serializes"),
    );
    assert_eq!(setting["error"]["kind"], "backend");
    assert!(message(&setting).contains("credential path"));
    assert!(
        !serde_json::to_string(&setting)
            .expect("response serializes")
            .contains(value)
    );
}

#[test]
fn backend_validation_is_reused_through_the_bridge() {
    // The shared backend validates before it resolves a provider, so these
    // paths stay hermetic while still proving the payload reaches it.
    let limit = handle_line(br#"{"id":1,"method":"get_tasks","params":{"request":{"limit":0}}}"#);
    assert_eq!(limit["error"]["kind"], "backend");
    assert!(message(&limit).contains("task limit must be between 1 and 50"));

    let state =
        handle_line(br#"{"id":2,"method":"get_tasks","params":{"request":{"state":"bogus"}}}"#);
    assert_eq!(state["error"]["kind"], "backend");
    assert!(message(&state).contains("task state must be open, closed, or all"));

    let role =
        handle_line(br#"{"id":3,"method":"get_status","params":{"request":{"role":"bogus"}}}"#);
    assert_eq!(role["error"]["kind"], "backend");
    assert!(message(&role).contains("invalid role 'bogus'"));

    // A missing required payload field fails as an argument error instead.
    let query = handle_line(br#"{"id":4,"method":"get_provisioning_status","params":{}}"#);
    assert_eq!(query["error"]["kind"], "argument");
    assert!(message(&query).contains("role"));
}

#[test]
fn responses_reuse_the_backend_payload_shape() {
    let response = handle_line(br#"{"id":1,"method":"get_app_metadata"}"#);
    let expected = serde_json::to_value(crate::gui::app_metadata()).expect("metadata serializes");
    assert_eq!(response["result"], expected);
    let encoded = serde_json::to_string(&response)
        .expect("response serializes")
        .to_ascii_lowercase();
    for forbidden in ["token", "secret", "password", "credential", "api-key"] {
        assert!(!encoded.contains(forbidden), "{forbidden} leaked");
    }
}

#[test]
fn responses_are_correlated_by_id_across_workers() {
    let mut input = Vec::new();
    for id in 0..8 {
        input.extend_from_slice(&request_line(
            json!({"id": id, "method": "get_app_metadata"}),
        ));
    }
    input.extend_from_slice(&request_line(
        json!({"id": "last", "method": "get_app_metadata"}),
    ));
    let (code, responses) = run_session(&input, WORKER_COUNT);
    assert_eq!(code, 0);
    assert_eq!(responses.len(), 9);
    for response in &responses {
        assert_eq!(response["ok"], true);
        assert_eq!(response["protocol"], PROTOCOL_VERSION);
        assert_eq!(response["result"]["name"], "phasegent");
    }
    for id in 0..8 {
        assert_eq!(responses.iter().filter(|item| item["id"] == id).count(), 1);
    }
    assert_eq!(
        responses.iter().filter(|item| item["id"] == "last").count(),
        1
    );
}

#[test]
fn eof_drains_and_exits_clean() {
    let (code, responses) = run_session(b"", 2);
    assert_eq!(code, 0);
    assert!(responses.is_empty());

    // A final request without a trailing newline is still served.
    let (code, responses) = run_session(br#"{"id":1,"method":"get_app_metadata"}"#, 2);
    assert_eq!(code, 0);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["ok"], true);
    assert_eq!(responses[0]["id"], 1);

    // CRLF framing is accepted.
    let (code, responses) = run_session(b"{\"id\":1,\"method\":\"get_app_metadata\"}\r\n", 1);
    assert_eq!(code, 0);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["ok"], true);
}

#[test]
fn line_reader_enforces_the_exact_bound() {
    let content = vec![b'a'; MAX_LINE_BYTES];
    let accepted = [
        content.clone(),
        [content.clone(), b"\n".to_vec()].concat(),
        [content.clone(), b"\r\n".to_vec()].concat(),
    ];
    for input in accepted {
        let mut reader = Cursor::new(input);
        match read_bounded_line(&mut reader, MAX_LINE_BYTES).expect("read line") {
            LineOutcome::Line(line) => assert_eq!(line.len(), MAX_LINE_BYTES),
            other => panic!("expected a bounded line, got {other:?}"),
        }
    }

    // An overlong line is rejected without swallowing the line after it.
    let input = [
        vec![b'a'; MAX_LINE_BYTES + 1],
        b"\n".to_vec(),
        request_line(json!({"id": 9, "method": "get_app_metadata"})),
    ]
    .concat();
    let mut reader = Cursor::new(input);
    assert!(matches!(
        read_bounded_line(&mut reader, MAX_LINE_BYTES).expect("read"),
        LineOutcome::Oversized
    ));
    match read_bounded_line(&mut reader, MAX_LINE_BYTES).expect("read next") {
        LineOutcome::Line(line) => assert!(line.ends_with(b"\"get_app_metadata\"}")),
        other => panic!("expected the next line, got {other:?}"),
    }
    assert!(matches!(
        read_bounded_line(&mut reader, MAX_LINE_BYTES).expect("eof"),
        LineOutcome::Eof
    ));
}

#[test]
fn run_rejects_unexpected_arguments() {
    assert_eq!(run(&["--help".to_owned()]), 2);
    assert_eq!(run(&["extra".to_owned(), "more".to_owned()]), 2);
}

#[test]
fn bridge_command_stays_hidden_from_the_cli_surface() {
    assert_eq!(COMMAND, "desktop-bridge");
    // Not a registered command: the parser reports it as unknown and help
    // never advertises it.
    let parsed = crate::command::parse(&[COMMAND.to_owned()]);
    assert!(parsed.is_err());
    assert!(!crate::command::top_level_visible(COMMAND, None, None));
}
