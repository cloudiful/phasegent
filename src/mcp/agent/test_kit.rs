//! A fake ACP agent used by the adapter tests.
//!
//! Implements the exact subset of MCode's ACP surface the adapter
//! consumes: `initialize` (with a configurable protocol version and
//! `loadSession` capability), `session/new` and `session/load`
//! (advertising the research permission mode, model, and effort),
//! `session/set_config_option` (rejecting values the fake did not
//! advertise and answering with the session's re-read advertisement so
//! the adapter's response verification has something real to check),
//! `session/prompt` (streams message chunks, optionally issues a
//! permission request, and honours a cancel that arrives mid-turn), and
//! the permission-request callback. Runs over an in-process duplex so
//! no real process, credential, or network is touched.

use std::sync::Arc;

use tokio::sync::Mutex;

use super::wire;

mod serve;
mod turn;

/// Scripted behavior knobs for the fake agent.
#[derive(Clone, Default)]
pub(crate) struct FakeAgentOptions {
    /// Drop the `thinkingEffort` option from the advertisement.
    pub omit_effort_option: bool,
    /// Drop the model option from the advertisement.
    pub omit_model_option: bool,
    /// Drop the `permissionMode` option from the advertisement.
    pub omit_permission_mode_option: bool,
    /// Reject every `session/set_config_option` call.
    pub reject_set_config: bool,
    /// Accept the set but report a stale `currentValue`, which is what
    /// a silently-ignored selection looks like on the wire.
    pub set_config_reports_stale_value: bool,
    /// Accept the set but report no advertisement at all.
    pub set_config_reports_nothing: bool,
    /// Advertise no config options in `session/new` or `session/load`.
    pub omit_all_config_options: bool,
    /// Advertise an ACP protocol version the adapter does not speak.
    pub protocol_version: Option<u32>,
    /// Omit `loadSession` from `agentCapabilities`.
    pub no_load_session: bool,
    /// Refuse `session/load`.
    pub reject_load_session: bool,
    /// Answer `session/load` with a different session id.
    pub load_returns_other_session: bool,
    /// Reply to the prompt with `cancelled` instead of `end_turn`
    /// after streaming.
    pub prompt_cancelled: bool,
    /// Never answer the prompt (for timeout tests).
    pub hang_prompt: bool,
    /// Finish the prompt only once `session/cancel` arrives, proving a
    /// cancel reaches an in-flight turn.
    pub prompt_awaits_cancel: bool,
    /// Issue one permission request mid-turn of `permission_kind` with
    /// the given paths, and stop the turn after the decision.
    pub permission_request: Option<PermissionScript>,
}

/// A scripted `session/request_permission` the fake issues mid-turn.
#[derive(Clone)]
pub(crate) struct PermissionScript {
    pub kind: &'static str,
    /// Tool input, including any path the scope check must inspect.
    pub raw_input: serde_json::Value,
    /// Locations the agent declares for the call.
    pub locations: Vec<String>,
}

impl PermissionScript {
    pub(crate) fn write(path: &str) -> Self {
        Self {
            kind: "edit",
            raw_input: serde_json::json!({ "path": path }),
            locations: vec![path.to_owned()],
        }
    }

    pub(crate) fn read(path: &str) -> Self {
        Self {
            kind: "read",
            raw_input: serde_json::json!({ "path": path }),
            locations: vec![path.to_owned()],
        }
    }

    pub(crate) fn search(pattern: &str, path: Option<&str>) -> Self {
        let mut raw_input = serde_json::json!({ "pattern": pattern });
        if let Some(path) = path {
            raw_input["path"] = serde_json::Value::String(path.to_owned());
        }
        Self {
            kind: "search",
            raw_input,
            locations: path.map(|value| vec![value.to_owned()]).unwrap_or_default(),
        }
    }

    /// A read whose escape is only visible in a nested input field, not
    /// in a declared location.
    pub(crate) fn read_nested_escape(pattern: &str, path: &str) -> Self {
        Self {
            kind: "search",
            raw_input: serde_json::json!({ "filter": { "include": [path] }, "pattern": pattern }),
            locations: Vec::new(),
        }
    }

    /// A network read that names one absolute URL.
    pub(crate) fn fetch(target: &str) -> Self {
        Self {
            kind: "fetch",
            raw_input: serde_json::json!({ "url": target }),
            locations: Vec::new(),
        }
    }

    /// A network read that names no target at all.
    pub(crate) fn fetch_without_a_target() -> Self {
        Self {
            kind: "fetch",
            raw_input: serde_json::json!({ "maxBytes": 4_096 }),
            locations: Vec::new(),
        }
    }
}

pub(crate) struct FakeAgent {
    pub(crate) options: FakeAgentOptions,
    /// Outgoing lines the agent produced.
    sent: Arc<Mutex<Vec<serde_json::Value>>>,
    /// Decisions the agent made for permission requests.
    pub(crate) permission_outcomes: Arc<Mutex<Vec<serde_json::Value>>>,
    /// `session/set_config_option` calls the agent accepted, as
    /// `(configId, value)` pairs, in order.
    pub(crate) selections: Arc<Mutex<Vec<(String, String)>>>,
    /// `session/load` calls the agent served, with the requested ids.
    pub(crate) loaded_sessions: Arc<Mutex<Vec<String>>>,
    /// Prompt text blocks the agent received, in order.
    pub(crate) prompts: Arc<Mutex<Vec<String>>>,
}

impl FakeAgent {
    pub(crate) fn new(options: FakeAgentOptions) -> Self {
        Self {
            options,
            sent: Arc::new(Mutex::new(Vec::new())),
            permission_outcomes: Arc::new(Mutex::new(Vec::new())),
            selections: Arc::new(Mutex::new(Vec::new())),
            loaded_sessions: Arc::new(Mutex::new(Vec::new())),
            prompts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Serve one adapter connection until the client side closes.
    pub(crate) async fn serve(self: Arc<Self>, client: tokio::io::DuplexStream) {
        serve::run(self, client).await;
    }

    /// The advertisement `session/new` and `session/load` share.
    pub(super) fn config_options(&self, current_effort: &str) -> Vec<serde_json::Value> {
        if self.options.omit_all_config_options {
            return Vec::new();
        }
        let mut config_options = Vec::new();
        if !self.options.omit_permission_mode_option {
            config_options.push(serde_json::json!({
                "type": "select",
                "id": wire::CONFIG_ID_PERMISSION_MODE,
                "name": "Permission mode",
                "currentValue": "auto",
                "options": [
                    {"value": "auto", "name": "Auto"},
                    {"value": wire::RESEARCH_PERMISSION_MODE, "name": "Default"},
                    {"value": "bypassPermissions", "name": "Full access"},
                ],
            }));
        }
        if !self.options.omit_model_option {
            config_options.push(serde_json::json!({
                "type": "select",
                "id": wire::CONFIG_ID_MODEL,
                "name": "Model",
                "currentValue": "m:minimax:MiniMax-M2.7:v:thinking",
                "options": [
                    {"value": "m:minimax:MiniMax-M2.7:v:thinking", "name": "M2.7"},
                    {"value": wire::RESEARCH_MODEL_WIRE_VALUE, "name": "M3.1 Flash"},
                ],
            }));
        }
        if !self.options.omit_effort_option {
            config_options.push(serde_json::json!({
                "type": "select",
                "id": wire::CONFIG_ID_THINKING_EFFORT,
                "name": "Thinking effort",
                "currentValue": current_effort,
                "options": [
                    {"value": "low", "name": "low"},
                    {"value": "medium", "name": "medium"},
                    {"value": wire::RESEARCH_THINKING_EFFORT, "name": "high"},
                ],
            }));
        }
        config_options
    }

    /// The advertisement MCode returns from `set_config_option`: the
    /// same controls with the accepted value now current.
    pub(super) fn config_options_with(
        &self,
        current_effort: &str,
        model: &str,
        permission_mode: &str,
    ) -> Vec<serde_json::Value> {
        let mut options = self.config_options(current_effort);
        for option in &mut options {
            match option["id"].as_str() {
                Some(wire::CONFIG_ID_MODEL) => {
                    option["currentValue"] = serde_json::Value::String(model.to_owned());
                }
                Some(wire::CONFIG_ID_PERMISSION_MODE) => {
                    option["currentValue"] = serde_json::Value::String(permission_mode.to_owned());
                }
                _ => {}
            }
        }
        options
    }
}
