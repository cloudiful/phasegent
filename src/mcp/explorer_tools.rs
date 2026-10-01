//! Role-gated explorer delegation over MCP (issue 685 P2, AC 3/4).
//!
//! Five tools — start, status, wait, cancel, resume — and nothing else. Each
//! one is a thin wire handler over the shared delegation contract in
//! [`crate::command::explorer`] and the ACP run manager in
//! [`crate::mcp::agent`]; this module owns the wire shapes and the
//! model-visible projection of a run, and nothing else.
//!
//! Three properties hold across all five.
//!
//! **The model never names where it runs.** The only caller-supplied locator
//! is the issue number, and it is a *selector*: [`BoundTarget::resolve`]
//! requires exactly one `active` worktree lease for that issue *and* the
//! host-bound session, and fails closed on both a missing and an ambiguous
//! result. There is no cwd argument, no path argument, and no fallback to the
//! server's own directory.
//!
//! **The session is host-supplied, not model-supplied.** Every tool takes a
//! `session` field that the OpenCode host bridge overwrites with its own
//! session id immediately before `tools/call`; a model-emitted value in that
//! field is never used, and a missing or malformed one is refused. The value is
//! validated and used as a lookup key only, and is never returned.
//!
//! **A run belongs to the session that started it.** Status, wait, cancel,
//! and resume resolve the durable owner binding first, so a foreign session
//! cannot read another session's transcript, wait on it, cancel it, or resume
//! it. The worktree path and the owner session are server-side data:
//! [`ExplorerRunView`] is the model-visible projection and neither has a field
//! in it.

use std::time::Duration;

use crate::command::explorer::{self, BoundTarget};
use crate::infra::storage::Storage;
use crate::mcp::agent::run::RunManager;
use crate::mcp::agent::store::{RESUMABLE_STATUSES, WorkRun, validate_run_id};
use crate::mcp::agent::{AcpSpawnConfig, ExplorerPrompt};
use rmcp::{
    ErrorData as McpError, handler::server::wrapper::Parameters, model::CallToolResult, tool,
    tool_router,
};

use super::tool_registry;
use super::tools::{PhasegentMcpServer, internal_error, invalid_params, ok_json};

/// The run-id prefix. Runs are named by the server, never by a caller, and the
/// prefix keeps a run id recognisable in a transcript without carrying the
/// issue or the session in it.
pub(crate) const RUN_ID_PREFIX: &str = "explorer";

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExplorerStartParams {
    /// Issue that selects the worktree. This session's single active lease for
    /// the issue is the only directory the run may use.
    issue: u64,
    /// Host-bound session id. The OpenCode host bridge overwrites this field
    /// with its own session; a model-supplied value is never trusted.
    session: String,
    /// Read-only recon prompt.
    prompt: String,
    /// Optional per-turn budget in seconds; clamped to the adapter's ceiling.
    timeout_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExplorerRunParams {
    /// Run id returned by `explorer_start`.
    run_id: String,
    /// Host-bound session id, injected by the host bridge.
    session: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExplorerWaitParams {
    /// Run id returned by `explorer_start`.
    run_id: String,
    /// Host-bound session id, injected by the host bridge.
    session: String,
    /// Optional wait budget in seconds; clamped to the delegation ceiling.
    timeout_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExplorerResumeParams {
    /// Run id returned by `explorer_start`.
    run_id: String,
    /// Host-bound session id, injected by the host bridge.
    session: String,
    /// Follow-up prompt for the resumed ACP session.
    prompt: String,
    /// Optional per-turn budget in seconds; clamped to the adapter's ceiling.
    timeout_secs: Option<u64>,
}

/// The model-visible view of one run.
///
/// A projection rather than the stored row: the worktree cwd and the owner
/// session are server-side data and have no field here, so a result cannot
/// leak them even if the row is serialized by accident. `output` is the
/// bounded transcript, `output_truncated` says whether it was cut, and
/// `terminal` says whether the run can still change — a `wait` that ran out of
/// budget returns `terminal: false` rather than implying a finished run.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ExplorerRunView {
    run_id: String,
    status: String,
    terminal: bool,
    /// Whether this run still has a resumable ACP session.
    resumable: bool,
    output: Option<String>,
    output_truncated: bool,
    error: Option<String>,
}

impl ExplorerRunView {
    pub(crate) fn of(run: &WorkRun) -> Self {
        let terminal = RunManager::is_terminal(run);
        Self {
            run_id: run.run_id.clone(),
            status: run.status.clone(),
            terminal,
            resumable: RESUMABLE_STATUSES.contains(&run.status.as_str()),
            output: run.output.clone(),
            output_truncated: run.output_truncated,
            error: run.error.clone(),
        }
    }

    pub(crate) fn payload(run: &WorkRun) -> serde_json::Value {
        serde_json::json!({ "run": Self::of(run) })
    }
}

#[tool_router(router = explorer_tool_router, vis = "pub(crate)")]
impl PhasegentMcpServer {
    #[tool(
        name = "explorer_start",
        description = "Start one read-only explorer run for an issue in this session's worktree. Returns a run id immediately; use explorer_wait or explorer_status for the result. The worktree is resolved server-side from this session's active lease and is never returned."
    )]
    async fn explorer_start(
        &self,
        Parameters(params): Parameters<ExplorerStartParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::EXPLORER_START)?;
        let session = host_session(&params.session)?;
        let (prompt, budget) =
            explorer::validate_prompt(&params.prompt, params.timeout_secs).map_err(bad_request)?;
        let storage = Storage::open().map_err(internal)?;
        let target = BoundTarget::resolve(&storage, params.issue, &session)
            .map_err(|error| internal(error.message()))?;
        let manager = self.explorer_manager()?;
        let run_id = new_run_id();
        let started = manager
            .start_run(
                &run_id,
                &session,
                AcpSpawnConfig::new(target.worktree),
                ExplorerPrompt::new(prompt).with_timeout_secs(budget),
            )
            .await
            .map_err(|error| internal(error.message))?;
        ok_json(&ExplorerRunView::payload(&started))
    }

    #[tool(
        name = "explorer_status",
        description = "Read one explorer run's current status and bounded output. Only the session that started the run may read it."
    )]
    async fn explorer_status(
        &self,
        Parameters(params): Parameters<ExplorerRunParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::EXPLORER_STATUS)?;
        let session = host_session(&params.session)?;
        let run = self
            .explorer_manager()?
            .owned_run(&run_id(&params.run_id)?, &session)
            .map_err(internal)?;
        ok_json(&ExplorerRunView::payload(&run))
    }

    #[tool(
        name = "explorer_wait",
        description = "Wait up to timeout_secs for an explorer run to finish, then return its state. A wait that runs out of budget returns the run as still active; wait again or read explorer_status."
    )]
    async fn explorer_wait(
        &self,
        Parameters(params): Parameters<ExplorerWaitParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::EXPLORER_WAIT)?;
        let session = host_session(&params.session)?;
        let budget = Duration::from_secs(explorer::resolve_wait_budget(params.timeout_secs));
        let run = self
            .explorer_manager()?
            .wait_run(&run_id(&params.run_id)?, &session, budget)
            .await
            .map_err(internal)?;
        ok_json(&ExplorerRunView::payload(&run))
    }

    #[tool(
        name = "explorer_cancel",
        description = "Cancel an explorer run this session started. The run becomes terminal and its ACP process is stopped."
    )]
    async fn explorer_cancel(
        &self,
        Parameters(params): Parameters<ExplorerRunParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::EXPLORER_CANCEL)?;
        let session = host_session(&params.session)?;
        let run = self
            .explorer_manager()?
            .cancel_owned_run(&run_id(&params.run_id)?, &session)
            .await
            .map_err(internal)?;
        ok_json(&ExplorerRunView::payload(&run))
    }

    #[tool(
        name = "explorer_resume",
        description = "Resume a failed, cancelled, timed-out, or interrupted explorer run this session started, continuing its saved ACP session with a follow-up prompt. A completed run cannot be resumed."
    )]
    async fn explorer_resume(
        &self,
        Parameters(params): Parameters<ExplorerResumeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::EXPLORER_RESUME)?;
        let session = host_session(&params.session)?;
        let (prompt, budget) =
            explorer::validate_prompt(&params.prompt, params.timeout_secs).map_err(bad_request)?;
        let run = self
            .explorer_manager()?
            .resume_owned_run(
                &run_id(&params.run_id)?,
                &session,
                ExplorerPrompt::new(prompt).with_timeout_secs(budget),
            )
            .await
            .map_err(internal)?;
        ok_json(&ExplorerRunView::payload(&run))
    }

    /// The process-wide manager for this server's database.
    ///
    /// Sharing it per database rather than per server instance is what keeps a
    /// run reachable across the streamable-HTTP transport, where every client
    /// connection builds a new server. The returned manager is cheap to clone
    /// and holds no per-call state.
    fn explorer_manager(&self) -> Result<RunManager, McpError> {
        let storage = Storage::open().map_err(internal)?;
        crate::mcp::agent::run::shared(&storage).map_err(internal)
    }
}

/// A fresh, server-generated run id. Ids never carry the issue or the session,
/// so a run id that reaches a transcript identifies nothing.
pub(crate) fn new_run_id() -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{RUN_ID_PREFIX}-{stamp:x}-{:x}", std::process::id())
}

/// The caller's run id, validated before it reaches the ledger.
fn run_id(raw: &str) -> Result<String, McpError> {
    validate_run_id(raw).map_err(bad_request)?;
    Ok(raw.trim().to_owned())
}

/// The host-bound session, validated. A blank, over-long, or control-character
/// value is refused before anything is read: the field is a lookup key, so an
/// unusable one must never fall through to a default.
fn host_session(raw: &str) -> Result<String, McpError> {
    explorer::validate_session(raw).map_err(bad_request)
}

fn bad_request(raw: impl AsRef<str>) -> McpError {
    invalid_params(raw.as_ref())
}

/// Storage and manager failures are internal, not the caller's fault: the
/// message is bounded and never carries a path, a session, or a credential.
fn internal(raw: impl AsRef<str>) -> McpError {
    internal_error(raw.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: &str) -> WorkRun {
        WorkRun {
            run_id: "explorer-1".to_owned(),
            status: status.to_owned(),
            worktree_cwd: "/secret/worktree".to_owned(),
            acp_session_id: Some("acp-1".to_owned()),
            prompt: "recon".to_owned(),
            output: Some("evidence".to_owned()),
            output_truncated: false,
            error: None,
            created_at: 1,
            updated_at: 2,
            finished_at: Some(2),
        }
    }

    /// The model-visible view is a projection, not the stored row: the worktree
    /// cwd and the ACP session id are server-side data and have no field in it,
    /// and a run that is still active is never reported as finished.
    #[test]
    fn the_run_view_carries_no_path_and_no_session_id() {
        let payload = ExplorerRunView::payload(&run("completed"));
        let text = payload.to_string();
        assert!(!text.contains("/secret/worktree"), "{text}");
        assert!(!text.contains("acp-1"), "{text}");
        assert!(!text.contains("worktree_cwd"), "{text}");
        assert_eq!(payload["run"]["terminal"], serde_json::json!(true));
        assert_eq!(payload["run"]["resumable"], serde_json::json!(false));

        let active = ExplorerRunView::payload(&WorkRun {
            status: "running".to_owned(),
            finished_at: None,
            ..run("completed")
        });
        assert_eq!(active["run"]["terminal"], serde_json::json!(false));
        assert_eq!(active["run"]["resumable"], serde_json::json!(false));

        let interrupted = ExplorerRunView::payload(&WorkRun {
            status: "interrupted".to_owned(),
            ..run("completed")
        });
        assert_eq!(interrupted["run"]["resumable"], serde_json::json!(true));
    }

    /// Run ids are server-generated and carry neither the issue nor the
    /// session, so an id that reaches a transcript identifies nothing.
    #[test]
    fn generated_run_ids_carry_no_issue_or_session() {
        let id = new_run_id();
        assert!(id.starts_with(RUN_ID_PREFIX), "{id}");
        assert!(!id.contains("685"), "{id}");
        assert!(!id.contains("ses_"), "{id}");
        assert_ne!(id, new_run_id(), "run ids must not repeat");
        validate_run_id(&id).expect("a generated run id is valid");
    }

    /// The host session is validated as a lookup key before it reaches the
    /// ledger, and a caller-supplied run id is bounded the same way.
    #[test]
    fn the_session_and_run_id_are_validated_at_the_boundary() {
        assert_eq!(host_session(" ses_x ").unwrap(), "ses_x");
        for raw in [
            "",
            "   ",
            "ses\n1",
            &"s".repeat(explorer::MAX_SESSION_CHARS + 1),
        ] {
            assert!(host_session(raw).is_err(), "session {raw:?}");
        }
        assert!(run_id("").is_err());
        assert!(run_id("bad\x01id").is_err());
        assert_eq!(run_id("  explorer-1 ").unwrap(), "explorer-1");
    }
}
