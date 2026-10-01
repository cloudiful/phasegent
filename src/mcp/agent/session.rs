//! One ACP process bound to one research session.
//!
//! [`AcpSession::start`] spawns `mcode acp` with the run's private
//! scratch directory as the process cwd and creates a session;
//! [`AcpSession::resume`] reconnects to a persisted ACP session id in a
//! fresh process through `session/load`, which is what makes an
//! interrupted run continuable rather than merely recorded. Both share
//! one lifecycle, so the process is killed and reaped on every exit —
//! failed handshake, aborted run, explicit [`AcpSession::kill`] — and
//! never left holding the scratch cwd.
//!
//! The child inherits an explicit environment allowlist, never the
//! phasegent server's own credentials. No lock is held across an
//! `await`: every request path clones the codec connection out of its
//! slot first, so a `session/cancel` reaches the agent while the
//! `session/prompt` it interrupts is still in flight.
//!
//! Config-option negotiation lives in [`negotiate`]; permission and
//! filesystem callbacks stay in [`super::callbacks`].

use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

#[cfg(test)]
use super::callbacks::wire_callbacks_over;
use super::callbacks::{SharedCore, Wired, wire_callbacks};
use super::codec::Connection;
use super::error::{AgentError, AgentResult};
use super::scope::WorkspaceScope;
use super::stream::{StderrTail, drain_stderr};
use super::types::{
    AcpSpawnConfig, NegotiatedReport, PromptOutcome, ResearchPrompt, StopReason, Transcript,
};
use super::wire;

mod handshake;
mod negotiate;

use handshake::{establish, spawn_child, with_diagnostics};

static NEXT_REQUEST_ID: AtomicI64 = AtomicI64::new(1);

pub(crate) fn next_request_id() -> i64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// One live ACP process serving one research session.
pub struct AcpSession {
    core: Arc<Mutex<SharedCore>>,
    /// A cheap clone of the codec connection. Request paths take a
    /// clone and drop this lock before awaiting, so concurrent traffic
    /// — a cancel during a prompt — never serializes behind the turn.
    connection: Mutex<Option<Connection>>,
    /// `None` for the test-only in-process transport, which has no
    /// process; its reader ends when the fake agent drops the duplex.
    child: Mutex<Option<tokio::process::Child>>,
    /// Reader and stderr-drain tasks, aborted on teardown.
    tasks: Mutex<Vec<JoinHandle<()>>>,
    stderr_tail: StderrTail,
}

impl std::fmt::Debug for AcpSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpSession")
            .field(
                "session_id",
                &self.core.lock().ok().map(|core| core.session_id.clone()),
            )
            .finish_non_exhaustive()
    }
}

impl AcpSession {
    /// Spawn the process, run the ACP handshake, and create the
    /// session. Fail-closed request/notification callbacks are wired
    /// before any agent traffic can arrive.
    pub async fn start(config: &AcpSpawnConfig) -> AgentResult<Self> {
        Self::open(config, None).await
    }

    /// Reconnect to a persisted ACP session id in a fresh process.
    /// Fails closed unless the agent advertised `loadSession` on
    /// `initialize` and answers `session/load` with the requested
    /// session id plus a config-option advertisement this adapter can
    /// re-verify its pinned values against.
    pub async fn resume(config: &AcpSpawnConfig, session_id: &str) -> AgentResult<Self> {
        Self::open(config, Some(session_id)).await
    }

    async fn open(config: &AcpSpawnConfig, resume: Option<&str>) -> AgentResult<Self> {
        let scope = WorkspaceScope::new(&config.cwd);
        let mut child = spawn_child(config)?;
        let (stderr_task, tail) = match child.stderr.take() {
            Some(stderr) => drain_stderr(stderr),
            None => (tokio::spawn(async {}), StderrTail::default()),
        };
        let wired = match wire_callbacks(&mut child, scope.clone()) {
            Ok(wired) => wired,
            Err(error) => {
                drop(child);
                stderr_task.abort();
                return Err(error);
            }
        };
        match Self::handshake(wired, &config.cwd, resume, config, &tail).await {
            Ok(session) => Ok(session.attach(child, stderr_task, tail)),
            Err(error) => {
                // `handshake` already closed the transport and ended
                // the reader; dropping a `kill_on_drop` handle reaps
                // the process, so nothing is orphaned here.
                drop(child);
                stderr_task.abort();
                Err(error)
            }
        }
    }

    /// Test-only constructor over an in-process duplex stream: the
    /// codec reads and writes the stream while a fake agent serves the
    /// far side. No process is involved.
    #[cfg(test)]
    pub(crate) async fn start_in_process(
        stream: tokio::io::DuplexStream,
        cwd: String,
    ) -> AgentResult<Self> {
        let (read, write) = tokio::io::split(stream);
        Self::open_in_process(read, write, &cwd, None).await
    }

    /// Test-only resume over an in-process duplex stream.
    #[cfg(test)]
    pub(crate) async fn resume_in_process(
        stream: tokio::io::DuplexStream,
        cwd: String,
        session_id: &str,
    ) -> AgentResult<Self> {
        let (read, write) = tokio::io::split(stream);
        Self::open_in_process(read, write, &cwd, Some(session_id)).await
    }

    #[cfg(test)]
    async fn open_in_process(
        read: tokio::io::ReadHalf<tokio::io::DuplexStream>,
        write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        cwd: &str,
        resume: Option<&str>,
    ) -> AgentResult<Self> {
        let scope = WorkspaceScope::new(Path::new(cwd));
        let tail = StderrTail::default();
        Self::handshake(
            wire_callbacks_over(read, write, scope),
            Path::new(cwd),
            resume,
            &AcpSpawnConfig::new(cwd),
            &tail,
        )
        .await
    }

    fn attach(
        mut self,
        child: tokio::process::Child,
        stderr_task: JoinHandle<()>,
        stderr_tail: StderrTail,
    ) -> Self {
        *self.child.lock().expect("child lock") = Some(child);
        self.tasks.lock().expect("tasks lock").push(stderr_task);
        self.stderr_tail = stderr_tail;
        self
    }

    /// Run the ACP handshake and build the session around the wired
    /// codec. `resume` selects `session/load` instead of `session/new`.
    /// Every failure path closes the transport and ends the reader, so
    /// a rejected handshake never leaves a live process behind.
    async fn handshake(
        wired: Wired,
        cwd: &Path,
        resume: Option<&str>,
        config: &AcpSpawnConfig,
        tail: &StderrTail,
    ) -> AgentResult<Self> {
        let Wired {
            connection,
            reader_task,
            core,
        } = wired;
        let advertised = match establish(&connection, cwd, resume, config, tail).await {
            Ok(advertised) => advertised,
            Err(error) => {
                connection.close().await;
                reader_task.abort();
                return Err(error);
            }
        };
        {
            let mut shared = core.lock().expect("session core lock");
            shared.session_id = advertised.session_id;
            shared.config_options = advertised.config_options;
        }
        Ok(Self {
            core,
            connection: Mutex::new(Some(connection)),
            child: Mutex::new(None),
            tasks: Mutex::new(vec![reader_task]),
            stderr_tail: tail.clone(),
        })
    }

    /// A cheap clone of the codec connection, taken without holding a
    /// guard across the caller's `await`.
    fn connection(&self) -> AgentResult<Connection> {
        self.connection
            .lock()
            .expect("connection lock")
            .as_ref()
            .cloned()
            .ok_or_else(AgentError::closed)
    }

    /// Run one request against the stored connection.
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> AgentResult<serde_json::Value> {
        self.connection()?
            .request(next_request_id, method, params)
            .await
    }

    /// Send one notification against the stored connection.
    async fn notify(&self, method: &str, params: serde_json::Value) -> AgentResult<()> {
        self.connection()?.notify(method, params).await
    }

    pub async fn session_id(&self) -> String {
        self.core
            .lock()
            .expect("session core lock")
            .session_id
            .clone()
    }

    /// The config options the agent advertised on `session/new` or
    /// `session/load`.
    pub async fn advertised_config_options(&self) -> Vec<wire::ConfigOption> {
        self.core
            .lock()
            .expect("session core lock")
            .config_options
            .clone()
    }

    /// The values the agent itself reported back for each config
    /// option this adapter selected. `None` until a negotiation
    /// response confirmed them.
    pub async fn negotiated(&self) -> NegotiatedReport {
        let shared = self.core.lock().expect("session core lock");
        NegotiatedReport {
            permission_mode: shared.permission_mode.clone(),
            model: shared.model.clone(),
            effort: shared.effort.clone(),
        }
    }

    /// Adopt one agent-confirmed selection and the option list the
    /// agent re-read, so the recorded state is the agent's own.
    fn record_selection(&self, config_id: &str, value: &str, options: Vec<wire::ConfigOption>) {
        self.core
            .lock()
            .expect("session core lock")
            .record_selection(config_id, value, options);
    }

    /// The workspace root every allowed read is bounded to.
    pub fn scope(&self) -> WorkspaceScope {
        self.core.lock().expect("session core lock").scope.clone()
    }

    /// Run one prompt turn to completion, streaming message chunks
    /// into a fresh transcript, and enforce the timeout. The wire text is
    /// the fixed server-owned [`super::types::RESEARCH_INSTRUCTION`] preamble
    /// followed by the caller's user request, so the caller can never supply
    /// or drop the system half. A timeout leaves the turn in flight until
    /// cancellation or process exit, so callers follow a timeout with
    /// [`AcpSession::kill`].
    pub async fn prompt(&self, prompt: &ResearchPrompt) -> AgentResult<PromptOutcome> {
        let session_id = self.session_id().await;
        {
            let shared = self.core.lock().expect("session core lock");
            *shared.transcript.lock().expect("transcript lock") = Transcript::default();
        }
        let started = std::time::Instant::now();
        let wire_text = prompt.wire_text();
        let request = self.request(
            wire::METHOD_SESSION_PROMPT,
            serde_json::to_value(wire::PromptParams {
                sessionId: &session_id,
                prompt: [wire::PromptBlock {
                    kind: "text",
                    text: &wire_text,
                }],
            })
            .expect("serialize prompt"),
        );
        match tokio::time::timeout(prompt.effective_timeout(), request).await {
            Err(_) => Err(AgentError::timeout()),
            Ok(Err(error)) => Err(with_diagnostics(error, &self.stderr_tail)),
            Ok(Ok(payload)) => {
                let outcome: wire::PromptResult = serde_json::from_value(payload)
                    .map_err(|error| AgentError::protocol(format!("bad prompt result: {error}")))?;
                let elapsed = started.elapsed().as_secs();
                let (text, truncated) = self.transcript_snapshot();
                Ok(PromptOutcome {
                    stop_reason: StopReason::parse(outcome.stopReason.as_deref()),
                    text,
                    truncated,
                    elapsed_secs: elapsed,
                })
            }
        }
    }

    fn transcript_snapshot(&self) -> (String, bool) {
        let shared = self.core.lock().expect("session core lock");
        let transcript = shared.transcript.lock().expect("transcript lock");
        transcript.snapshot()
    }

    /// Snapshot and drain the transcript of an in-flight turn. Used by
    /// the cancel path so partial research output survives.
    pub async fn take_transcript(&self) -> (String, bool) {
        let shared = self.core.lock().expect("session core lock");
        let mut transcript = shared.transcript.lock().expect("transcript lock");
        let snapshot = transcript.snapshot();
        *transcript = Transcript::default();
        snapshot
    }

    /// Cancel the in-flight prompt turn; the pending `session/prompt`
    /// request then resolves with `stopReason: "cancelled"`. This
    /// never waits on the prompt's own request slot, so it reaches the
    /// agent while that turn is still running.
    pub async fn cancel(&self) -> AgentResult<()> {
        let session_id = self.session_id().await;
        self.notify(
            wire::METHOD_SESSION_CANCEL,
            serde_json::to_value(wire::CancelParams {
                sessionId: &session_id,
            })
            .expect("serialize cancel"),
        )
        .await
    }

    /// Kill the process, reap the reader and stderr tasks, and close
    /// the transport. Idempotent, and free of any guard held across an
    /// `await`, so it works while a prompt is in flight.
    pub async fn kill(&self) {
        let child = self.child.lock().expect("child lock").take();
        if let Some(mut child) = child {
            let _ = child.kill().await;
        }
        let tasks = std::mem::take(&mut *self.tasks.lock().expect("tasks lock"));
        for task in tasks {
            task.abort();
        }
        let connection = self.connection.lock().expect("connection lock").take();
        if let Some(connection) = connection {
            connection.close().await;
        }
    }
}
