//! Role-gated research delegation over MCP (issue 692 P1).
//!
//! Five tools — start, status, wait, cancel, resume — and nothing else. Each
//! one is a thin wire handler over the shared delegation contract in
//! [`crate::command::research`] and the ACP run manager in
//! [`crate::mcp::agent`]; this module owns the wire shapes and the
//! model-visible projection of a run, and nothing else.
//!
//! Three properties hold across all five.
//!
//! **The model never names where it runs.** There is no issue parameter, no
//! cwd argument, no path argument, and no worktree or lease lookup. The run
//! manager creates a private server-side scratch directory per attempt; the
//! path stays in the server-side database and never appears in an argument, a
//! result, or a log line.
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
//! it. The scratch path and the owner session are server-side data:
//! [`ResearchRunView`] is the model-visible projection and neither has a field
//! in it.
//!
//! The caller supplies only the research user prompt; the fixed read-only
//! system instruction is added by the ACP adapter and cannot be replaced
//! through any argument.

use std::time::Duration;

use crate::command::research;
use crate::infra::storage::Storage;
use crate::mcp::agent::run::RunManager;
use crate::mcp::agent::store::{RESUMABLE_STATUSES, WorkRun, validate_run_id};
use crate::mcp::agent::{AcpSpawnConfig, ResearchPrompt};
use rmcp::{
    ErrorData as McpError, handler::server::wrapper::Parameters, model::CallToolResult, tool,
    tool_router,
};

use super::tool_registry;
use super::tools::{PhasegentMcpServer, internal_error, invalid_params, ok_json};

/// The run-id prefix. Runs are named by the server, never by a caller, and the
/// prefix keeps a run id recognisable in a transcript without carrying any
/// caller or location data.
pub(crate) const RUN_ID_PREFIX: &str = "research";

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ResearchStartParams {
    /// Host-bound session id. The OpenCode host bridge overwrites this field
    /// with its own session; a model-supplied value is never trusted.
    session: String,
    /// The research user prompt. The server adds the fixed read-only system
    /// instruction; this is the only user request.
    prompt: String,
    /// Optional per-turn budget in seconds; clamped to the adapter's ceiling.
    timeout_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ResearchRunParams {
    /// Run id returned by `research_start`.
    run_id: String,
    /// Host-bound session id, injected by the host bridge.
    session: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ResearchWaitParams {
    /// Run id returned by `research_start`.
    run_id: String,
    /// Host-bound session id, injected by the host bridge.
    session: String,
    /// Optional wait budget in seconds; clamped to the delegation ceiling.
    timeout_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ResearchResumeParams {
    /// Run id returned by `research_start`.
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
/// A projection rather than the stored row: the scratch cwd and the owner
/// session are server-side data and have no field here, so a result cannot
/// leak them even if the row is serialized by accident. `output` is the
/// bounded transcript, `output_truncated` says whether it was cut, and
/// `terminal` says whether the run can still change — a `wait` that ran out of
/// budget returns `terminal: false` rather than implying a finished run.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ResearchRunView {
    run_id: String,
    status: String,
    terminal: bool,
    /// Whether this run still has a resumable ACP session.
    resumable: bool,
    output: Option<String>,
    output_truncated: bool,
    error: Option<String>,
}

impl ResearchRunView {
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

#[tool_router(router = research_tool_router, vis = "pub(crate)")]
impl PhasegentMcpServer {
    #[tool(
        name = "research_start",
        description = "Start one read-only research run from a user prompt in a private server-side scratch directory. Returns a run id immediately; use research_wait or research_status for the result. No issue, worktree, or path is accepted or returned."
    )]
    async fn research_start(
        &self,
        Parameters(params): Parameters<ResearchStartParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::RESEARCH_START)?;
        let session = host_session(&params.session)?;
        let (prompt, budget) =
            research::validate_prompt(&params.prompt, params.timeout_secs).map_err(bad_request)?;
        let manager = self.research_manager()?;
        let run_id = new_run_id();
        let started = manager
            .start_run(
                &run_id,
                &session,
                AcpSpawnConfig::research(),
                ResearchPrompt::new(prompt).with_timeout_secs(budget),
            )
            .await
            .map_err(|error| internal(error.message))?;
        ok_json(&ResearchRunView::payload(&started))
    }

    #[tool(
        name = "research_status",
        description = "Read one research run's current status and bounded output. Only the session that started the run may read it."
    )]
    async fn research_status(
        &self,
        Parameters(params): Parameters<ResearchRunParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::RESEARCH_STATUS)?;
        let session = host_session(&params.session)?;
        let run = self
            .research_manager()?
            .owned_run(&run_id(&params.run_id)?, &session)
            .map_err(internal)?;
        ok_json(&ResearchRunView::payload(&run))
    }

    #[tool(
        name = "research_wait",
        description = "Wait up to timeout_secs for a research run to finish, then return its state. A wait that runs out of budget returns the run as still active; wait again or read research_status."
    )]
    async fn research_wait(
        &self,
        Parameters(params): Parameters<ResearchWaitParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::RESEARCH_WAIT)?;
        let session = host_session(&params.session)?;
        let budget = Duration::from_secs(research::resolve_wait_budget(params.timeout_secs));
        let run = self
            .research_manager()?
            .wait_run(&run_id(&params.run_id)?, &session, budget)
            .await
            .map_err(internal)?;
        ok_json(&ResearchRunView::payload(&run))
    }

    #[tool(
        name = "research_cancel",
        description = "Cancel a research run this session started. The run becomes terminal and its ACP process is stopped."
    )]
    async fn research_cancel(
        &self,
        Parameters(params): Parameters<ResearchRunParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::RESEARCH_CANCEL)?;
        let session = host_session(&params.session)?;
        let run = self
            .research_manager()?
            .cancel_owned_run(&run_id(&params.run_id)?, &session)
            .await
            .map_err(internal)?;
        ok_json(&ResearchRunView::payload(&run))
    }

    #[tool(
        name = "research_resume",
        description = "Resume a failed, cancelled, timed-out, or interrupted research run this session started, continuing its saved ACP session in a fresh scratch directory with a follow-up prompt. A completed run cannot be resumed."
    )]
    async fn research_resume(
        &self,
        Parameters(params): Parameters<ResearchResumeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.gate(tool_registry::RESEARCH_RESUME)?;
        let session = host_session(&params.session)?;
        let (prompt, budget) =
            research::validate_prompt(&params.prompt, params.timeout_secs).map_err(bad_request)?;
        let run = self
            .research_manager()?
            .resume_owned_run(
                &run_id(&params.run_id)?,
                &session,
                ResearchPrompt::new(prompt).with_timeout_secs(budget),
            )
            .await
            .map_err(internal)?;
        ok_json(&ResearchRunView::payload(&run))
    }

    /// The process-wide manager for this server's database.
    ///
    /// Sharing it per database rather than per server instance is what keeps a
    /// run reachable across the streamable-HTTP transport, where every client
    /// connection builds a new server. The returned manager is cheap to clone
    /// and holds no per-call state.
    fn research_manager(&self) -> Result<RunManager, McpError> {
        let storage = Storage::open().map_err(internal)?;
        crate::mcp::agent::run::shared(&storage).map_err(internal)
    }
}

/// A fresh, server-generated run id. Ids carry no caller or location data, so
/// a run id that reaches a transcript identifies nothing.
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
    research::validate_session(raw).map_err(bad_request)
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
            run_id: "research-1".to_owned(),
            status: status.to_owned(),
            scratch_cwd: "/secret/scratch".to_owned(),
            acp_session_id: Some("acp-1".to_owned()),
            prompt: "research".to_owned(),
            output: Some("evidence".to_owned()),
            output_truncated: false,
            error: None,
            created_at: 1,
            updated_at: 2,
            finished_at: Some(2),
        }
    }

    /// The model-visible view is a projection, not the stored row: the scratch
    /// cwd and the ACP session id are server-side data and have no field in it,
    /// and a run that is still active is never reported as finished.
    #[test]
    fn the_run_view_carries_no_path_and_no_session_id() {
        let payload = ResearchRunView::payload(&run("completed"));
        let text = payload.to_string();
        assert!(!text.contains("/secret/scratch"), "{text}");
        assert!(!text.contains("acp-1"), "{text}");
        assert!(!text.contains("scratch_cwd"), "{text}");
        assert_eq!(payload["run"]["terminal"], serde_json::json!(true));
        assert_eq!(payload["run"]["resumable"], serde_json::json!(false));

        let active = ResearchRunView::payload(&WorkRun {
            status: "running".to_owned(),
            finished_at: None,
            ..run("completed")
        });
        assert_eq!(active["run"]["terminal"], serde_json::json!(false));
        assert_eq!(active["run"]["resumable"], serde_json::json!(false));

        let interrupted = ResearchRunView::payload(&WorkRun {
            status: "interrupted".to_owned(),
            ..run("completed")
        });
        assert_eq!(interrupted["run"]["resumable"], serde_json::json!(true));
    }

    /// Run ids are server-generated and carry no caller or location data, so an
    /// id that reaches a transcript identifies nothing.
    #[test]
    fn generated_run_ids_carry_no_issue_or_session() {
        let id = new_run_id();
        assert!(id.starts_with(RUN_ID_PREFIX), "{id}");
        assert!(!id.contains("ses_"), "{id}");
        assert!(!id.contains('/'), "{id}");
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
            &"s".repeat(research::MAX_SESSION_CHARS + 1),
        ] {
            assert!(host_session(raw).is_err(), "session {raw:?}");
        }
        assert!(run_id("").is_err());
        assert!(run_id("bad\x01id").is_err());
        assert_eq!(run_id("  research-1 ").unwrap(), "research-1");
    }
}
