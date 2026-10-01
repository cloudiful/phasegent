//! The fake agent's connection loop.
//!
//! Split from the option/record surface in the parent module because
//! this is the part that tracks protocol state across requests: which
//! session methods were called, what the current model and permission
//! mode are, and whether a cancel landed while a prompt was in flight.
//! The turn body itself lives in [`super::turn`].

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

use super::super::wire;
use super::FakeAgent;
use super::turn::run_turn;

/// Session state the fake tracks across requests.
pub(super) struct SessionState {
    pub(super) permission_mode: String,
    pub(super) model: String,
    pub(super) effort: String,
    /// Set by `session/cancel` while a prompt is in flight.
    pub(super) cancelled: bool,
}

pub(super) async fn run(agent: Arc<FakeAgent>, client: DuplexStream) {
    let (reader_stream, mut writer) = tokio::io::split(client);
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();
    let mut next_id = 1000_i64;
    let mut state = SessionState {
        permission_mode: "auto".to_owned(),
        model: "m:minimax:MiniMax-M2.7:v:thinking".to_owned(),
        effort: "low".to_owned(),
        cancelled: false,
    };
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let method = message["method"].as_str().unwrap_or("").to_owned();
        let params = message["params"].clone();
        let id = message["id"].as_i64();
        if method == wire::METHOD_SESSION_CANCEL {
            // A real agent ends the active turn here; recording it is
            // what lets a test prove a cancel reached the agent.
            state.cancelled = true;
            continue;
        }
        let response = match (method.as_str(), id) {
            (wire::METHOD_INITIALIZE, Some(id)) => {
                let mut capabilities = serde_json::json!({});
                if !agent.options.no_load_session {
                    capabilities["loadSession"] = serde_json::Value::Bool(true);
                }
                serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "protocolVersion": agent.options.protocol_version.unwrap_or(1),
                        "agentCapabilities": capabilities,
                    }
                })
            }
            (wire::METHOD_SESSION_NEW, Some(id)) => {
                let response = serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "sessionId": "fake-session-1",
                        "configOptions": agent.config_options(&state.effort),
                    }
                });
                response
            }
            (wire::METHOD_SESSION_LOAD, Some(id)) => {
                if agent.options.reject_load_session {
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32602, "message": "session is not loadable"},
                    })
                } else {
                    let requested = params["sessionId"].as_str().unwrap_or("").to_owned();
                    agent.loaded_sessions.lock().await.push(requested.clone());
                    let session_id = if agent.options.load_returns_other_session {
                        "fake-session-2".to_owned()
                    } else {
                        requested
                    };
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "sessionId": session_id,
                            "configOptions": agent.config_options(&state.effort),
                        }
                    })
                }
            }
            (wire::METHOD_SESSION_SET_CONFIG_OPTION, Some(id)) => {
                set_config(&agent, &mut state, params, id).await
            }
            (wire::METHOD_SESSION_PROMPT, Some(id)) => {
                let turn = run_turn(
                    &agent,
                    &mut state,
                    &mut reader,
                    &mut writer,
                    &mut line,
                    &mut next_id,
                    params,
                    id,
                )
                .await;
                // A hung agent answers nothing at all; the adapter must
                // time out rather than read a response.
                let Some(turn) = turn else { continue };
                turn
            }
            (method, Some(id)) => serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("fake agent lacks {method}")},
            }),
            _ => continue,
        };
        agent.sent.lock().await.push(response.clone());
        let ok = writer
            .write_all(response.to_string().as_bytes())
            .await
            .is_ok()
            && writer.write_all(b"\n").await.is_ok()
            && writer.flush().await.is_ok();
        if !ok {
            break;
        }
    }
}

/// Mirror MCode's own advertisement check, then answer with the
/// session's re-read advertisement so the adapter can verify the
/// selection from the response instead of assuming it landed.
async fn set_config(
    agent: &Arc<FakeAgent>,
    state: &mut SessionState,
    params: serde_json::Value,
    id: i64,
) -> serde_json::Value {
    let config_id = params["configId"].as_str().unwrap_or("").to_owned();
    let value = params["value"].as_str().unwrap_or("").to_owned();
    let advertised = agent.config_options(&state.effort);
    let offered = advertised
        .iter()
        .find(|option| option["id"].as_str() == Some(config_id.as_str()))
        .is_some_and(|option| {
            option["options"].as_array().is_some_and(|choices| {
                choices
                    .iter()
                    .any(|choice| choice["value"].as_str() == Some(value.as_str()))
            })
        });
    if agent.options.reject_set_config || !offered {
        return serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "error": {"code": -32602, "message": "not advertised"},
        });
    }
    match config_id.as_str() {
        wire::CONFIG_ID_PERMISSION_MODE => state.permission_mode = value.clone(),
        wire::CONFIG_ID_MODEL => state.model = value.clone(),
        wire::CONFIG_ID_THINKING_EFFORT => state.effort = value.clone(),
        _ => {}
    }
    agent
        .selections
        .lock()
        .await
        .push((config_id.clone(), value.clone()));
    if agent.options.set_config_reports_nothing {
        return serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}});
    }
    let reported = if agent.options.set_config_reports_stale_value {
        agent.config_options(&state.effort)
    } else {
        agent.config_options_with(&state.effort, &state.model, &state.permission_mode)
    };
    serde_json::json!({
        "jsonrpc": "2.0", "id": id,
        "result": {"configOptions": reported},
    })
}
