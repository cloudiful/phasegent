//! ACP wire types for the agent-to-client half of the protocol.
//!
//! These are the requests and notifications MCode sends *to* this
//! client: the permission request that gates every tool call, and the
//! streaming update notifications. Kept apart from
//! [`super::wire`] because the two directions are answered by
//! different code with different trust: one is chosen by this client,
//! the other is produced by the model.
//!
//! Field names deliberately mirror the ACP JSON wire spelling.
#![allow(non_snake_case)]

use serde::Deserialize;

/// `session/request_permission` incoming request.
#[derive(Clone, Debug, Deserialize)]
pub struct RequestPermissionParams {
    #[serde(default)]
    pub sessionId: Option<String>,
    #[serde(default)]
    pub toolCall: Option<ToolCallSummary>,
    #[serde(default)]
    pub options: Vec<PermissionOption>,
}

impl RequestPermissionParams {
    /// First deny-kind option id, preferring `reject_once`. Fails
    /// closed: when the agent advertises no deny option the caller
    /// cancels the request instead of selecting anything permissive.
    pub fn deny_option_id(&self) -> Option<String> {
        self.options
            .iter()
            .find(|option| option.kind_is("reject_once"))
            .or_else(|| {
                self.options
                    .iter()
                    .find(|option| option.kind_is("reject_always"))
            })
            .and_then(|option| option.optionId.clone())
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ToolCallSummary {
    #[serde(default)]
    pub kind: Option<String>,
    /// Files the agent says the call touches; part of the scope check.
    #[serde(default)]
    pub locations: Vec<ToolCallLocation>,
    /// The tool's own input, from which the scope check also reads the
    /// paths a tool names without declaring a location.
    #[serde(default)]
    pub rawInput: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolCallLocation {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PermissionOption {
    #[serde(default)]
    pub optionId: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

impl PermissionOption {
    fn kind_is(&self, kind: &str) -> bool {
        self.kind.as_deref() == Some(kind)
    }
}

/// Permission response payloads.
pub fn permission_selected_json(option_id: &str) -> serde_json::Value {
    serde_json::json!({"outcome": {"outcome": "selected", "optionId": option_id}})
}

pub fn permission_cancelled_json() -> serde_json::Value {
    serde_json::json!({"outcome": {"outcome": "cancelled"}})
}

/// `session/update` notification params, decoded for the fields this
/// adapter forwards into the transcript.
#[derive(Clone, Debug, Deserialize)]
pub struct SessionUpdateParams {
    #[serde(default)]
    pub sessionId: Option<String>,
    #[serde(default)]
    pub update: Option<serde_json::Value>,
}

impl SessionUpdateParams {
    /// Extract streamed assistant text from an
    /// `agent_message_chunk` update. Everything else (plan, tool
    /// calls, thought chunks) is ignored for the result transcript.
    pub fn message_chunk_text(&self) -> Option<String> {
        let update = self.update.as_ref()?;
        if update.get("sessionUpdate")?.as_str()? != "agent_message_chunk" {
            return None;
        }
        let content = update.get("content")?;
        if content.get("type")?.as_str()? != "text" {
            return None;
        }
        content
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }
}
