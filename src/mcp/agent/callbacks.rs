//! Fail-closed callbacks the codec invokes for agent-initiated traffic.
//!
//! `session/request_permission` follows the read-only research
//! contract on two axes. The tool *kind* must be a read: `read`,
//! `search`, and `fetch` may proceed, and every mutation kind
//! (`edit`, `delete`, `move`, `execute`, `switch_mode`, anything
//! unknown) is denied. The *scope* must also hold: an allowed kind
//! whose paths leave the scratch workspace is denied too, so a `read` cannot
//! reach the phasegent credential store or the agent's own
//! configuration. Denials prefer `reject_once`, fall back to
//! `reject_always`, and cancel outright when the agent advertises no
//! deny option, so nothing permissive is ever selected silently.
//!
//! `session/update` notifications append streamed assistant text to
//! the shared transcript. Anything else is refused as unimplemented,
//! matching the client capabilities the session advertised.

use std::sync::{Arc, Mutex};

use super::codec;
use super::scope::WorkspaceScope;
use super::types::Transcript;
use super::wire::{self, IncomingMessage};
use super::wire_client::{
    RequestPermissionParams, SessionUpdateParams, permission_cancelled_json,
    permission_selected_json,
};

/// Per-process shared state the codec callbacks touch. Std locks are
/// used because callbacks are sync; sections stay tiny and never
/// await while held.
pub(crate) struct SharedCore {
    pub(crate) session_id: String,
    pub(crate) config_options: Vec<wire::ConfigOption>,
    /// Values the agent reported back for the config options this
    /// adapter set. Client-side intent is deliberately not recorded.
    pub(crate) permission_mode: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) scope: WorkspaceScope,
    pub(crate) transcript: Mutex<Transcript>,
}

impl SharedCore {
    pub(crate) fn new(scope: WorkspaceScope) -> Self {
        Self {
            session_id: String::new(),
            config_options: Vec::new(),
            permission_mode: None,
            model: None,
            effort: None,
            scope,
            transcript: Mutex::new(Transcript::default()),
        }
    }

    /// Record one agent-confirmed selection and adopt the option list
    /// the agent re-read, so later checks see its state and not ours.
    pub(crate) fn record_selection(
        &mut self,
        config_id: &str,
        value: &str,
        options: Vec<wire::ConfigOption>,
    ) {
        let slot = match config_id {
            wire::CONFIG_ID_PERMISSION_MODE => &mut self.permission_mode,
            wire::CONFIG_ID_MODEL => &mut self.model,
            wire::CONFIG_ID_THINKING_EFFORT => &mut self.effort,
            _ => return,
        };
        *slot = Some(value.to_owned());
        if !options.is_empty() {
            self.config_options = options;
        }
    }
}

/// A codec connection, its reader task, and the core the callbacks
/// share: the pieces a session is built from.
pub(crate) struct Wired {
    pub(crate) connection: codec::Connection,
    pub(crate) reader_task: tokio::task::JoinHandle<()>,
    pub(crate) core: Arc<Mutex<SharedCore>>,
}

/// Wire the fail-closed callbacks onto a spawned child's stdio.
pub(crate) fn wire_callbacks(
    child: &mut tokio::process::Child,
    scope: WorkspaceScope,
) -> Result<Wired, super::error::AgentError> {
    let core = Arc::new(Mutex::new(SharedCore::new(scope)));
    let (connection, reader_task) = codec::Connection::start(child, callbacks_for(&core))?;
    Ok(Wired {
        connection,
        reader_task,
        core,
    })
}

/// Test-only wiring over an in-process duplex pair; the halves move
/// into the codec's writer/reader tasks.
#[cfg(test)]
pub(crate) fn wire_callbacks_over(
    read: impl tokio::io::AsyncRead + Send + Unpin + 'static,
    write: impl tokio::io::AsyncWrite + Send + Unpin + 'static,
    scope: WorkspaceScope,
) -> Wired {
    let core = Arc::new(Mutex::new(SharedCore::new(scope)));
    let (connection, reader_task) =
        codec::Connection::start_over(read, write, callbacks_for(&core));
    Wired {
        connection,
        reader_task,
        core,
    }
}

/// Build the request/notification callbacks bound to one session core.
pub(crate) fn callbacks_for(
    core: &Arc<Mutex<SharedCore>>,
) -> (codec::AgentRequestCallback, codec::NotificationCallback) {
    let request_core = core.clone();
    let agent_requests: codec::AgentRequestCallback =
        Arc::new(move |method, params| handle_agent_request(&request_core, method, params));
    let notify_core = core.clone();
    let notifications: codec::NotificationCallback =
        Arc::new(move |message| handle_notification(&notify_core, message));
    (agent_requests, notifications)
}

/// Permission decisions follow the read-only research contract;
/// everything else is refused as unimplemented.
fn handle_agent_request(
    core: &Arc<Mutex<SharedCore>>,
    method: &str,
    params: Option<&serde_json::Value>,
) -> Result<serde_json::Value, (i64, String)> {
    if method != wire::METHOD_SESSION_REQUEST_PERMISSION {
        return Err(codec::method_not_found_error(method));
    }
    // An undecodable request denies: the fields that decide the answer
    // are exactly the ones that failed to parse.
    let parsed: RequestPermissionParams = params
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .ok_or((-32602, "unreadable permission request".to_owned()))?;
    let deny = || {
        Ok(match parsed.deny_option_id() {
            Some(id) => permission_selected_json(&id),
            None => permission_cancelled_json(),
        })
    };
    let Some(call) = parsed.toolCall.as_ref() else {
        return deny();
    };
    let kind = call.kind.as_deref().unwrap_or("");
    if !super::is_read_only_kind(kind) {
        return deny();
    }
    let in_scope = core
        .lock()
        .expect("session core lock")
        .scope
        .permits_read(call);
    if !in_scope {
        return deny();
    }
    match parsed
        .options
        .iter()
        .find(|option| option.kind.as_deref() == Some("allow_once"))
        .and_then(|option| option.optionId.clone())
    {
        Some(id) => Ok(permission_selected_json(&id)),
        // No allow option advertised: refusing is the only safe answer,
        // and an error beats a silent hang for the agent.
        None => Err((-32602, "no allow option advertised".to_owned())),
    }
}

/// Stream assistant message chunks into the shared transcript.
fn handle_notification(core: &Arc<Mutex<SharedCore>>, message: &IncomingMessage) {
    if message.method.as_deref() != Some(wire::METHOD_SESSION_UPDATE) {
        return;
    }
    let Some(params) = &message.params else {
        return;
    };
    let Ok(update) = serde_json::from_value::<SessionUpdateParams>(params.clone()) else {
        return;
    };
    if let Some(text) = update.message_chunk_text() {
        core.lock()
            .expect("session core lock")
            .transcript
            .lock()
            .expect("transcript lock")
            .push_chunk(&text);
    }
}
