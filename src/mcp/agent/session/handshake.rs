//! The ACP handshake: process spawn, `initialize`, and the session call.
//!
//! Split from the session lifecycle so the boundary that can leave a
//! live process behind sits in one file. Every failure path here ends
//! the transport, and the spawn is `kill_on_drop`, so a rejected
//! handshake can never orphan an `mcode acp` process still holding the
//! worktree cwd.

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use super::super::codec::Connection;
use super::super::error::{AgentError, AgentResult};
use super::super::stream::StderrTail;
use super::super::types::AcpSpawnConfig;
use super::super::wire::{self, SessionNewResult};
use super::next_request_id;

/// What a successful handshake learned from the agent's responses.
pub(super) struct Advertised {
    pub(super) session_id: String,
    pub(super) config_options: Vec<wire::ConfigOption>,
}

/// `initialize`, then `session/new` or `session/load`, verifying the
/// protocol version and the load capability before either session call.
/// The whole exchange is bounded: an agent that never answers must not
/// hold its worktree cwd, and the run's cancel escalation, forever.
pub(super) async fn establish(
    connection: &Connection,
    cwd: &Path,
    resume: Option<&str>,
    config: &AcpSpawnConfig,
    tail: &StderrTail,
) -> AgentResult<Advertised> {
    tokio::time::timeout(
        config.effective_handshake_timeout(),
        establish_inner(connection, cwd, resume, tail),
    )
    .await
    .unwrap_or_else(|_| Err(AgentError::timeout()))
}

async fn establish_inner(
    connection: &Connection,
    cwd: &Path,
    resume: Option<&str>,
    tail: &StderrTail,
) -> AgentResult<Advertised> {
    let initialize = connection
        .request(
            next_request_id,
            wire::METHOD_INITIALIZE,
            serde_json::json!({
                "protocolVersion": wire::SUPPORTED_PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": {"readTextFile": false, "writeTextFile": false},
                    "terminal": false,
                },
            }),
        )
        .await
        .map_err(|error| with_diagnostics(error, tail))?;
    let initialize: wire::InitializeResult = serde_json::from_value(initialize)
        .map_err(|error| AgentError::protocol(format!("bad initialize result: {error}")))?;
    let version = initialize
        .protocol_version()
        .map_err(AgentError::negotiation)?;
    if version != wire::SUPPORTED_PROTOCOL_VERSION {
        return Err(AgentError::negotiation(format!(
            "agent speaks ACP protocol version {version}"
        )));
    }
    if let Some(session_id) = resume
        && session_id.trim().is_empty()
    {
        return Err(AgentError::negotiation("no ACP session id to resume"));
    }
    if resume.is_some() && !initialize.load_session() {
        return Err(AgentError::negotiation(
            "agent does not advertise session/load",
        ));
    }
    let (method, params) = match resume {
        Some(session_id) => (
            wire::METHOD_SESSION_LOAD,
            serde_json::json!({
                "sessionId": session_id,
                "cwd": cwd.display().to_string(),
                "mcpServers": [],
            }),
        ),
        None => (
            wire::METHOD_SESSION_NEW,
            serde_json::json!({
                "cwd": cwd.display().to_string(),
                "mcpServers": [],
            }),
        ),
    };
    let created = connection
        .request(next_request_id, method, params)
        .await
        .map_err(|error| with_diagnostics(error, tail))?;
    let created: SessionNewResult = serde_json::from_value(created)
        .map_err(|error| AgentError::protocol(format!("bad session result: {error}")))?;
    if let Some(expected) = resume
        && created.sessionId != expected
    {
        return Err(AgentError::negotiation(
            "session/load returned a different session id",
        ));
    }
    if created.configOptions.is_empty() {
        return Err(AgentError::negotiation(
            "agent advertised no session config options",
        ));
    }
    Ok(Advertised {
        session_id: created.sessionId,
        config_options: created.configOptions,
    })
}

/// Attach the agent's own diagnostics to a failure so a handshake or
/// turn death is diagnosable instead of silent.
pub(super) fn with_diagnostics(error: AgentError, tail: &StderrTail) -> AgentError {
    match tail.text() {
        Some(text) => AgentError::new(error.kind, format!("{}: {text}", error.message)),
        None => error,
    }
}

/// Spawn `mcode acp` with the phasegent worktree as its cwd and a
/// cleared environment carrying only the allowlist. `kill_on_drop`
/// closes the orphan path: an aborted run task or a failed handshake
/// drops the handle and the process dies with it.
pub(super) fn spawn_child(config: &AcpSpawnConfig) -> AgentResult<tokio::process::Child> {
    let program = super::super::spawn_env::resolve_program(&config.program)?;
    let mut command = Command::new(program);
    command
        .arg("acp")
        .current_dir(&config.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear()
        .envs(super::super::spawn_env::child_env());
    command.spawn().map_err(|error| {
        AgentError::spawn(format!("could not spawn {} acp: {error}", config.program))
    })
}
