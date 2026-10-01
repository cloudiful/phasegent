//! The fake agent's prompt turn.
//!
//! Split out of the connection loop because the turn is where the
//! agent behaves like an agent rather than a JSON-RPC server: it
//! streams chunks, gates a tool call on a permission request, and holds
//! the turn open until a cancel arrives.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

use super::super::wire;
use super::FakeAgent;
use super::serve::SessionState;

/// Stream the turn, optionally issue one permission request, then
/// answer. `prompt_awaits_cancel` holds the turn open until a
/// `session/cancel` notification arrives, and `hang_prompt` never
/// answers at all.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_turn(
    agent: &Arc<FakeAgent>,
    state: &mut SessionState,
    reader: &mut BufReader<tokio::io::ReadHalf<DuplexStream>>,
    writer: &mut tokio::io::WriteHalf<DuplexStream>,
    line: &mut String,
    next_id: &mut i64,
    params: serde_json::Value,
    id: i64,
) -> Option<serde_json::Value> {
    if agent.options.hang_prompt {
        return None;
    }
    if agent.options.prompt_awaits_cancel {
        state.cancelled = false;
        // Only the cancel ends this turn. The read below is what makes
        // a missing cancel observable: the test would hang instead.
        loop {
            if state.cancelled {
                break;
            }
            line.clear();
            match reader.read_line(line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let Ok(reply) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if reply["method"].as_str() == Some(wire::METHOD_SESSION_CANCEL) {
                state.cancelled = true;
            }
        }
        return Some(serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"stopReason": "cancelled"},
        }));
    }
    state.cancelled = false;
    for text in ["found ", "12 modules", " in src/"] {
        let update = serde_json::json!({
            "jsonrpc": "2.0",
            "method": wire::METHOD_SESSION_UPDATE,
            "params": {
                "sessionId": params["sessionId"],
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": text},
                },
            },
        });
        agent.sent.lock().await.push(update.clone());
        writer
            .write_all(update.to_string().as_bytes())
            .await
            .expect("fake agent write");
        writer.write_all(b"\n").await.expect("fake agent write");
        writer.flush().await.expect("fake agent flush");
    }
    if let Some(script) = agent.options.permission_request.clone() {
        let request_id = *next_id;
        *next_id += 1;
        let locations: Vec<serde_json::Value> = script
            .locations
            .iter()
            .map(|path| serde_json::json!({"path": path}))
            .collect();
        let request = serde_json::json!({
            "jsonrpc": "2.0", "id": request_id,
            "method": wire::METHOD_SESSION_REQUEST_PERMISSION,
            "params": {
                "sessionId": params["sessionId"],
                "toolCall": {
                    "toolCallId": "t1",
                    "kind": script.kind,
                    "locations": locations,
                    "rawInput": script.raw_input,
                },
                "options": [
                    {"optionId": "allow-once", "name": "Allow", "kind": "allow_once"},
                    {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
                ],
            },
        });
        writer
            .write_all(request.to_string().as_bytes())
            .await
            .expect("fake agent write");
        writer.write_all(b"\n").await.expect("fake agent write");
        writer.flush().await.expect("fake agent flush");
        // Wait for the adapter's response line.
        loop {
            line.clear();
            let read = reader.read_line(line).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            let Ok(reply) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if reply["id"] == serde_json::json!(request_id) {
                agent
                    .permission_outcomes
                    .lock()
                    .await
                    .push(reply["result"].clone());
                break;
            }
        }
    }
    let stop = if agent.options.prompt_cancelled {
        "cancelled"
    } else {
        "end_turn"
    };
    Some(serde_json::json!({
        "jsonrpc": "2.0", "id": id,
        "result": {"stopReason": stop},
    }))
}
