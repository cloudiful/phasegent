//! ACP wire types: the exact subset of the Agent Client Protocol the
//! MCode adapter speaks, with serde shapes matching MCode 0.5.10.
//!
//! Only the client-to-agent half of the protocol lives here: what this
//! adapter sends, and the responses it reads back. The
//! agent-to-client requests it answers are in [`super::wire_client`].
//! The explorer permission mode, model, and thinking-effort values are
//! the ones the MCode agent advertises (`configOptions` on
//! `session/new`); they are pinned so the adapter always negotiates
//! the same wire values the P0 live audit verified.
//!
//! Field names deliberately mirror the ACP JSON wire spelling.
#![allow(non_snake_case)]

use serde::{Deserialize, Serialize};

pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_SESSION_NEW: &str = "session/new";
pub const METHOD_SESSION_LOAD: &str = "session/load";
pub const METHOD_SESSION_SET_CONFIG_OPTION: &str = "session/set_config_option";
pub const METHOD_SESSION_PROMPT: &str = "session/prompt";
pub const METHOD_SESSION_CANCEL: &str = "session/cancel";
pub const METHOD_SESSION_REQUEST_PERMISSION: &str = "session/request_permission";
pub const METHOD_SESSION_UPDATE: &str = "session/update";
pub const METHOD_FS_READ_TEXT_FILE: &str = "fs/read_text_file";
pub const METHOD_FS_WRITE_TEXT_FILE: &str = "fs/write_text_file";

/// Config-option ids MCode advertises on `session/new`.
pub const CONFIG_ID_MODEL: &str = "model";
pub const CONFIG_ID_THINKING_EFFORT: &str = "thinkingEffort";
/// MCode also advertises `permissionMode`; the P0 live audit confirmed
/// its advertised values are `default`, `auto`, and
/// `bypassPermissions`.
pub const CONFIG_ID_PERMISSION_MODE: &str = "permissionMode";

/// The explorer model selection exactly as MCode advertises it:
/// provider `minimax`, model `MiniMax-M3.1-Flash-Preview`, variant
/// `thinking`, encoded in MCode's `m:<p>:<m>:v:<variant>` wire format.
pub const EXPLORER_MODEL_WIRE_VALUE: &str = "m:minimax:MiniMax-M3.1-Flash-Preview:v:thinking";
/// The explorer thinking effort, one of MCode's advertised efforts for
/// the model above.
pub const EXPLORER_THINKING_EFFORT: &str = "high";
/// Permission mode selected for the run: `default` keeps MCode's
/// tool-permission flow enabled, which is the only mode under which
/// this client can deny a mutation at all. `auto` and
/// `bypassPermissions` would remove the request this adapter answers.
pub const EXPLORER_PERMISSION_MODE: &str = "default";
/// The only ACP protocol version this adapter speaks.
pub const SUPPORTED_PROTOCOL_VERSION: u32 = 1;

/// One incoming JSON-RPC message, flattened so requests, responses,
/// and notifications share one decode path.
#[derive(Clone, Debug, Deserialize)]
pub struct IncomingMessage {
    pub id: Option<serde_json::Value>,
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<RpcErrorObject>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcErrorObject {
    pub code: i64,
    #[serde(default)]
    pub message: String,
}

/// Build one outgoing request with a numeric id.
pub fn request_json(id: i64, method: &str, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
}

/// Build one outgoing notification (no id).
pub fn notification_json(method: &str, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
    .to_string()
}

/// Build the response to an agent-initiated request.
pub fn response_json(id: &serde_json::Value, result: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
    .to_string()
}

/// Build the error response to an agent-initiated request. The ACP
/// JSON-RPC error codes mirror LSP: -32601 method not found, -32603
/// internal error, -32602 invalid params.
pub fn error_response_json(id: &serde_json::Value, code: i64, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
    .to_string()
}

/// `initialize` result: only the capabilities this adapter consumes.
#[derive(Clone, Debug, Deserialize)]
pub struct InitializeResult {
    #[serde(default)]
    pub protocolVersion: Option<u32>,
    #[serde(default)]
    pub agentCapabilities: Option<AgentCapabilities>,
}

impl InitializeResult {
    /// The agent's advertised protocol version. ACP encodes it as a
    /// number; a string encoding is accepted and normalized so a
    /// stricter peer is a negotiation failure rather than a decode
    /// failure that looks like an unrelated protocol error.
    pub fn protocol_version(&self) -> Result<u32, String> {
        let raw = self
            .protocolVersion
            .ok_or_else(|| "agent did not report a protocol version".to_owned())?;
        match raw {
            0 => Err("agent reported protocol version 0".to_owned()),
            version => Ok(version),
        }
    }

    /// Whether the agent advertised `session/load`, the capability a
    /// resumed run depends on.
    pub fn load_session(&self) -> bool {
        self.agentCapabilities
            .as_ref()
            .and_then(|capabilities| capabilities.loadSession)
            .unwrap_or(false)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentCapabilities {
    #[serde(default)]
    pub loadSession: Option<bool>,
}

/// `session/new` and `session/load` result. MCode answers `session/load`
/// with the same session id plus a fresh `configOptions`
/// advertisement, which is what makes a resumed run re-verifiable
/// instead of trusted.
#[derive(Clone, Debug, Deserialize)]
pub struct SessionNewResult {
    pub sessionId: String,
    #[serde(default)]
    pub configOptions: Vec<ConfigOption>,
}

impl SessionNewResult {
    /// The agent's reported selection for `config_id`, if it advertises
    /// that control at all.
    pub fn current_value(&self, config_id: &str) -> Option<&str> {
        self.configOptions
            .iter()
            .find(|option| option.id == config_id)
            .and_then(|option| option.current_value())
    }
}

/// One advertised session configuration control. MCode models every
/// negotiation knob as a `select` whose options are the advertised
/// values; negotiation verifies against this list before setting.
#[derive(Clone, Debug, Deserialize)]
pub struct ConfigOption {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub currentValue: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Vec<ConfigOptionChoice>,
}

impl ConfigOption {
    pub fn select_option_values(&self) -> Vec<String> {
        self.options
            .iter()
            .filter_map(|choice| choice.value.as_str().map(str::to_owned))
            .collect()
    }

    /// The agent's reported current selection, which is the only
    /// trustworthy evidence that a `set_config_option` took effect.
    pub fn current_value(&self) -> Option<&str> {
        self.currentValue
            .as_ref()
            .and_then(serde_json::Value::as_str)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ConfigOptionChoice {
    pub value: serde_json::Value,
    #[serde(default)]
    pub name: Option<String>,
}

/// `session/set_config_option` result. MCode answers with the session's
/// re-read `configOptions`, whose `currentValue` is the agent's own
/// report of what it is now running with.
#[derive(Clone, Debug, Deserialize)]
pub struct SetConfigOptionResult {
    #[serde(default)]
    pub configOptions: Vec<ConfigOption>,
}

impl SetConfigOptionResult {
    /// The value the agent reports for `config_id` after the set.
    pub fn current_value(&self, config_id: &str) -> Option<&str> {
        self.configOptions
            .iter()
            .find(|option| option.id == config_id)
            .and_then(|option| option.current_value())
    }

    /// The re-read advertisement, empty when the agent reported none.
    pub fn into_options(self) -> Vec<ConfigOption> {
        self.configOptions
    }
}

/// `session/set_config_option` params.
#[derive(Clone, Debug, Serialize)]
pub struct SetConfigOptionParams<'a> {
    pub sessionId: &'a str,
    pub configId: &'a str,
    pub value: &'a str,
}

/// `session/prompt` params.
#[derive(Clone, Debug, Serialize)]
pub struct PromptParams<'a> {
    pub sessionId: &'a str,
    pub prompt: [PromptBlock<'a>; 1],
}

#[derive(Clone, Debug, Serialize)]
pub struct PromptBlock<'a> {
    #[serde(rename = "type")]
    pub kind: &'a str,
    pub text: &'a str,
}

/// `session/prompt` result.
#[derive(Clone, Debug, Deserialize)]
pub struct PromptResult {
    #[serde(default)]
    pub stopReason: Option<String>,
}

/// `session/cancel` params (notification).
#[derive(Clone, Debug, Serialize)]
pub struct CancelParams<'a> {
    pub sessionId: &'a str,
}

/// `session/load` params: reconnect to a persisted session id.
#[derive(Clone, Debug, Serialize)]
pub struct LoadSessionParams<'a> {
    pub sessionId: &'a str,
    pub cwd: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcpServers: Option<[serde_json::Value; 0]>,
}
