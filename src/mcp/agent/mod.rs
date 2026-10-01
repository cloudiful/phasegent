//! MCode ACP process/session adapter (issue 685 P1).
//!
//! One ACP process serves one active session: [`AcpSession`] spawns
//! `mcode acp` with the run's private scratch directory as the process cwd
//! and a credential-free environment, creates the session, negotiates
//! the research permission mode, model, and thinking effort through
//! `session/set_config_option` while verifying each value against the
//! agent's own response, and streams `session/update` notifications
//! into a bounded transcript. [`AcpSession::resume`] reconnects to a
//! persisted session id in a fresh process through `session/load`, so
//! an interrupted run is continuable rather than only recorded.
//!
//! A caller supplies only the user prompt: [`types::RESEARCH_INSTRUCTION`] is
//! a fixed server-owned read-only preamble the adapter prepends to every
//! prompt, so the caller cannot replace the system instruction.
//!
//! Tool-permission and filesystem callbacks fail closed on two axes:
//! mutation kinds are denied, and an allowed read kind is bounded — to
//! the scratch directory through the filesystem for `read` and `search`,
//! and to a credential-free absolute URL for `fetch` — so the phasegent
//! credential store and the agent's own configuration are unreachable
//! from a model-driven turn.
//!
//! The child's stderr is redacted where it is handed to a failure, and
//! again where any message becomes an [`AgentError`], so neither the
//! diagnostics stream nor an agent-authored JSON-RPC error message can
//! move a secret into a persisted or model-visible result.
//!
//! Run persistence lives in [`store`]: durable rows survive process
//! restarts and a run whose owning process died is detected as
//! interrupted, then resumed from its persisted ACP session id. The
//! manager keeps only run ids and child handles in memory; scratch
//! paths stay in the server-side database and are never returned in
//! results.
//!
//! A run also has a durable owner ([`store::owner`]): the host-bound
//! session that delegated it. Every read and every mutation the
//! delegation surface performs resolves that owner first
//! ([`RunManager::owned_run`], [`RunManager::wait_run`],
//! [`RunManager::cancel_owned_run`], [`RunManager::resume_owned_run`]),
//! so a run is only ever reachable by the session that started it. The
//! role-gated MCP operations that expose those methods live in
//! `mcp::research_tools`.
#![allow(dead_code)]

pub mod error;
pub mod run;
pub mod scope;
pub mod scratch;
pub mod spawn_env;
pub mod store;
pub mod stream;

mod callbacks;
mod codec;
mod contain;
mod fetch;
mod redact;
mod session;
mod types;
mod wire;
mod wire_client;

#[allow(unused_imports)]
pub use run::RunManager;
#[allow(unused_imports)]
pub use scope::WorkspaceScope;
#[allow(unused_imports)]
pub use session::AcpSession;
#[allow(unused_imports)]
pub use store::{WorkRun, WorkRunUpdate};
#[allow(unused_imports)]
pub use types::{
    AcpSpawnConfig, DEFAULT_PROMPT_TIMEOUT_SECS, HANDSHAKE_TIMEOUT_SECS, MAX_PROMPT_TIMEOUT_SECS,
    MAX_TRANSCRIPT_CHARS, NegotiatedReport, PermissionDecision, PromptOutcome,
    RESEARCH_INSTRUCTION, ResearchPrompt, StopReason, Transcript,
};

use error::AgentError;

/// The ACP tool kind that is a network read rather than a filesystem
/// read. It is read-only, but it names no path: it is bounded by
/// [`fetch`] instead of by the scratch workspace.
pub const KIND_FETCH: &str = "fetch";

/// ACP `ToolKind` values that only observe. `search` is the research
/// agent's primary tool — it is how a research prompt finds anything at
/// all — and [`KIND_FETCH`] is a read-only network access. All three stay
/// inside the read contract; every other kind, including `execute`, is a
/// mutation or an unknown and is denied.
pub fn is_read_only_kind(kind: &str) -> bool {
    matches!(kind, "read" | "search" | KIND_FETCH)
}

pub fn permission_decision_for_kind(kind: &str) -> PermissionDecision {
    if is_read_only_kind(kind) {
        PermissionDecision::Allow
    } else {
        PermissionDecision::Deny
    }
}

pub type AgentResult<T> = Result<T, AgentError>;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod store_resume_tests;

#[cfg(test)]
pub(crate) mod store_tests;

#[cfg(test)]
mod protocol_tests;

#[cfg(test)]
mod cancel_tests;

#[cfg(test)]
mod permission_tests;

#[cfg(test)]
mod process_tests;

#[cfg(test)]
mod resume_tests;

#[cfg(test)]
mod wire_tests;

#[cfg(test)]
mod owner_wait_tests;

#[cfg(test)]
pub(crate) mod test_kit;
