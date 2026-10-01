//! Wire-shape tests for the ACP types the research adapter speaks.
//!
//! These assert the exact JSON spellings MCode 0.5.10 puts on the
//! wire, so a change in the agent's shape fails here rather than in a
//! live handshake.

use super::wire::{
    CONFIG_ID_MODEL, CONFIG_ID_PERMISSION_MODE, CONFIG_ID_THINKING_EFFORT, IncomingMessage,
    InitializeResult, RESEARCH_MODEL_WIRE_VALUE, RESEARCH_PERMISSION_MODE,
    RESEARCH_THINKING_EFFORT, SUPPORTED_PROTOCOL_VERSION, SessionNewResult, SetConfigOptionResult,
};
use super::wire_client::{RequestPermissionParams, SessionUpdateParams, ToolCallSummary};

#[test]
fn incoming_message_distinguishes_request_response_notification() {
    let request: IncomingMessage = serde_json::from_str(
        r#"{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{}}"#,
    )
    .unwrap();
    assert_eq!(request.id, Some(serde_json::json!(7)));
    assert_eq!(
        request.method.as_deref(),
        Some("session/request_permission")
    );

    let response: IncomingMessage =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":3,"result":{"sessionId":"s"}}"#).unwrap();
    assert!(response.method.is_none());
    assert_eq!(response.result.unwrap()["sessionId"], "s");

    let notification: IncomingMessage = serde_json::from_str(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s"}}"#,
    )
    .unwrap();
    assert!(notification.id.is_none());
    assert!(notification.result.is_none());
}

#[test]
fn initialize_result_reports_protocol_version_and_load_capability() {
    let current: InitializeResult = serde_json::from_value(serde_json::json!({
        "protocolVersion": 1,
        "agentCapabilities": {"loadSession": true}
    }))
    .unwrap();
    assert_eq!(
        current.protocol_version().unwrap(),
        SUPPORTED_PROTOCOL_VERSION
    );
    assert!(current.load_session());

    let legacy: InitializeResult = serde_json::from_value(serde_json::json!({
        "protocolVersion": 1,
        "agentCapabilities": {"loadSession": false}
    }))
    .unwrap();
    assert!(!legacy.load_session());

    let bare: InitializeResult = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(
        bare.protocol_version().is_err(),
        "a missing version must fail closed"
    );
    assert!(!bare.load_session());
}

#[test]
fn set_config_option_result_reports_the_agents_own_current_value() {
    let reported: SetConfigOptionResult = serde_json::from_value(serde_json::json!({
        "configOptions": [
            {"type": "select", "id": CONFIG_ID_MODEL, "currentValue": RESEARCH_MODEL_WIRE_VALUE,
             "options": [{"value": RESEARCH_MODEL_WIRE_VALUE}]},
            {"type": "select", "id": CONFIG_ID_THINKING_EFFORT, "currentValue": "low",
             "options": [{"value": "low"}, {"value": RESEARCH_THINKING_EFFORT}]}
        ]
    }))
    .unwrap();
    assert_eq!(
        reported.current_value(CONFIG_ID_MODEL),
        Some(RESEARCH_MODEL_WIRE_VALUE)
    );
    assert_eq!(
        reported.current_value(CONFIG_ID_THINKING_EFFORT),
        Some("low"),
        "a stale currentValue is the signal negotiation must catch"
    );
    assert_eq!(reported.current_value(CONFIG_ID_PERMISSION_MODE), None);

    let silent: SetConfigOptionResult = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(
        silent.current_value(CONFIG_ID_MODEL).is_none(),
        "an agent that reports nothing cannot be verified"
    );
}

#[test]
fn load_result_carries_the_session_id_and_a_fresh_advertisement() {
    let loaded: SessionNewResult = serde_json::from_value(serde_json::json!({
        "sessionId": "ses-1",
        "configOptions": [
            {"type": "select", "id": CONFIG_ID_PERMISSION_MODE,
             "currentValue": RESEARCH_PERMISSION_MODE,
             "options": [{"value": RESEARCH_PERMISSION_MODE}, {"value": "auto"}]}
        ]
    }))
    .unwrap();
    assert_eq!(loaded.sessionId, "ses-1");
    assert_eq!(
        loaded.current_value(CONFIG_ID_PERMISSION_MODE),
        Some(RESEARCH_PERMISSION_MODE)
    );
}

#[test]
fn deny_option_prefers_reject_once_and_cancels_when_absent() {
    let parsed: RequestPermissionParams = serde_json::from_value(serde_json::json!({
        "options": [
            {"optionId": "a", "kind": "allow_once"},
            {"optionId": "d", "kind": "reject_once"},
            {"optionId": "e", "kind": "reject_always"}
        ]
    }))
    .unwrap();
    assert_eq!(parsed.deny_option_id().as_deref(), Some("d"));

    let only_allow: RequestPermissionParams = serde_json::from_value(serde_json::json!({
        "options": [{"optionId": "a", "kind": "allow_once"}]
    }))
    .unwrap();
    assert_eq!(only_allow.deny_option_id(), None);
}

#[test]
fn permission_params_decode_tool_call_scope_fields() {
    let parsed: RequestPermissionParams = serde_json::from_value(serde_json::json!({
        "sessionId": "ses-1",
        "toolCall": {
            "toolCallId": "t1",
            "kind": "search",
            "locations": [{"path": "src/main.rs", "line": 3}],
            "rawInput": {"pattern": "fn main", "path": "src"}
        },
        "options": []
    }))
    .unwrap();
    let call = parsed.toolCall.expect("tool call");
    assert_eq!(call.kind.as_deref(), Some("search"));
    assert_eq!(call.locations.len(), 1);
    assert_eq!(call.locations[0].path, "src/main.rs");
    assert!(call.rawInput.is_some());

    let bare: RequestPermissionParams =
        serde_json::from_value(serde_json::json!({"options": []})).unwrap();
    assert!(bare.toolCall.is_none());
    let empty = ToolCallSummary::default();
    assert!(empty.kind.is_none());
    assert!(empty.rawInput.is_none());
}

#[test]
fn message_chunk_text_extracts_text_chunks_only() {
    let chunk: SessionUpdateParams = serde_json::from_value(serde_json::json!({
        "sessionId": "s",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "hello"}
        }
    }))
    .unwrap();
    assert_eq!(chunk.message_chunk_text().as_deref(), Some("hello"));

    let plan: SessionUpdateParams = serde_json::from_value(serde_json::json!({
        "sessionId": "s",
        "update": {"sessionUpdate": "plan_update", "entries": []}
    }))
    .unwrap();
    assert_eq!(plan.message_chunk_text(), None);
}

#[test]
fn wire_values_pin_the_negotiated_model_and_effort() {
    assert_eq!(
        RESEARCH_MODEL_WIRE_VALUE,
        "m:minimax:MiniMax-M3.1-Flash-Preview:v:thinking"
    );
    assert_eq!(RESEARCH_THINKING_EFFORT, "high");
    assert_eq!(CONFIG_ID_MODEL, "model");
    assert_eq!(CONFIG_ID_THINKING_EFFORT, "thinkingEffort");
    assert_eq!(CONFIG_ID_PERMISSION_MODE, "permissionMode");
    assert_eq!(RESEARCH_PERMISSION_MODE, "default");
    assert_eq!(SUPPORTED_PROTOCOL_VERSION, 1);
}
