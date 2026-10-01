//! Value types for the ACP research adapter: spawn configuration,
//! bounded transcript, prompt input/output, and the read-only
//! permission decision.

use std::path::PathBuf;
use std::time::Duration;

/// Default wall-clock budget for one prompt turn.
pub const DEFAULT_PROMPT_TIMEOUT_SECS: u64 = 600;
/// Hard ceiling for a caller-supplied timeout override.
pub const MAX_PROMPT_TIMEOUT_SECS: u64 = 3600;
/// Wall-clock budget for `initialize` plus the session call. Without
/// it a process that never answers would hold its scratch cwd and the
/// run's cancel escalation open indefinitely.
pub const HANDSHAKE_TIMEOUT_SECS: u64 = 60;
/// Transcript cap in characters; oldest text drops first.
pub const MAX_TRANSCRIPT_CHARS: usize = 64_000;

/// Spawn configuration for one ACP process. Server-side only: the
/// program, the run's scratch cwd, and the handshake budget never reach a
/// model-visible argument or result.
#[derive(Clone, Debug)]
pub struct AcpSpawnConfig {
    /// Program to run (typically `mcode`); overridable for tests.
    pub program: String,
    /// Working directory: the private scratch directory the run manager
    /// assigns to this attempt. [`crate::mcp::agent::RunManager::start_run`]
    /// replaces any caller-supplied value with a fresh scratch path.
    pub cwd: PathBuf,
    /// Overrides [`HANDSHAKE_TIMEOUT_SECS`] for this run.
    pub handshake_timeout_secs: Option<u64>,
}

impl AcpSpawnConfig {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: "mcode".to_owned(),
            cwd: cwd.into(),
            handshake_timeout_secs: None,
        }
    }

    /// The default research spawn config. The scratch cwd is assigned by the
    /// run manager, so the caller never names it.
    pub fn research() -> Self {
        Self::new(PathBuf::new())
    }

    /// Replace the working directory, used by the run manager to pin the
    /// server-created scratch path for this attempt.
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    /// Clamp an override into a sane window; `None` keeps the default.
    pub fn with_handshake_timeout_secs(mut self, secs: u64) -> Self {
        self.handshake_timeout_secs = Some(secs.clamp(1, MAX_PROMPT_TIMEOUT_SECS));
        self
    }

    pub(crate) fn effective_handshake_timeout(&self) -> Duration {
        Duration::from_secs(
            self.handshake_timeout_secs
                .unwrap_or(HANDSHAKE_TIMEOUT_SECS)
                .clamp(1, MAX_PROMPT_TIMEOUT_SECS),
        )
    }
}

/// A permission decision for one agent tool-call request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionDecision {
    Allow,
    Deny,
}

/// Bounded prompt transcript. Chunks append; when the cap is exceeded
/// the oldest text drops and a truncation marker is recorded so a
/// reader can tell the result was cut.
#[derive(Default)]
pub struct Transcript {
    text: String,
    truncated: bool,
}

impl Transcript {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub(crate) fn snapshot(&self) -> (String, bool) {
        (self.text.clone(), self.truncated)
    }

    pub(crate) fn push_chunk(&mut self, chunk: &str) {
        self.text.push_str(chunk);
        while self.text.chars().count() > MAX_TRANSCRIPT_CHARS {
            let drop = (self.text.chars().count() - MAX_TRANSCRIPT_CHARS).min(1024);
            self.text = self.text.chars().skip(drop).collect();
            self.truncated = true;
        }
    }
}

/// Prompt exit reason, matching the ACP stop vocabulary this adapter
/// distinguishes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    EndTurn,
    Cancelled,
    Other,
}

impl StopReason {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("end_turn") => Self::EndTurn,
            Some("cancelled") => Self::Cancelled,
            _ => Self::Other,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::Cancelled => "cancelled",
            Self::Other => "other",
        }
    }
}

/// One completed prompt turn.
#[derive(Clone, Debug)]
pub struct PromptOutcome {
    pub stop_reason: StopReason,
    /// Final bounded transcript text.
    pub text: String,
    pub truncated: bool,
    pub elapsed_secs: u64,
}

/// The selection the agent itself reported back for each config option
/// this adapter sets. Every field is `None` until a negotiation
/// response confirmed that value, so a client-side echo can never make
/// this look complete.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NegotiatedReport {
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl NegotiatedReport {
    /// Whether every pinned wire value was confirmed by the agent.
    pub fn is_complete(&self) -> bool {
        self.permission_mode.is_some() && self.model.is_some() && self.effort.is_some()
    }
}

/// The fixed, server-owned read-only research instruction prepended to every
/// caller prompt. Callers supply only the user request; the adapter always
/// sends this preamble first, so a caller cannot replace or weaken the
/// read-only contract through arguments. It names no path and carries no
/// credential.
pub const RESEARCH_INSTRUCTION: &str = "You are a phasegent read-only research agent. \
Answer the user's research request using only read-only observation. \
Do not modify any file, repository, ref, commit, tag, or phasegent/provider state; \
do not run commands that mutate state; do not read, copy, or disclose credentials, \
tokens, auth payloads, or private keys; do not reveal filesystem paths. \
Keep the evidence bounded and the answer concise and factual.";

/// One research prompt: the caller's user request plus the per-run budget. The
/// fixed [`RESEARCH_INSTRUCTION`] is added by [`ResearchPrompt::wire_text`], so
/// the caller can never supply or drop the system half.
#[derive(Clone, Debug)]
pub struct ResearchPrompt {
    pub text: String,
    pub timeout_secs: Option<u64>,
}

impl ResearchPrompt {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            timeout_secs: None,
        }
    }

    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs.clamp(1, MAX_PROMPT_TIMEOUT_SECS));
        self
    }

    /// The text block actually sent to the ACP agent: the fixed server-owned
    /// instruction followed by the caller's user request.
    pub(crate) fn wire_text(&self) -> String {
        format!("{RESEARCH_INSTRUCTION}\n\n{}", self.text)
    }

    pub(crate) fn effective_timeout(&self) -> Duration {
        Duration::from_secs(
            self.timeout_secs
                .unwrap_or(DEFAULT_PROMPT_TIMEOUT_SECS)
                .clamp(1, MAX_PROMPT_TIMEOUT_SECS),
        )
    }
}
