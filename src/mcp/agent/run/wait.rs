//! Bounded waiting for an explorer run and the owner check every
//! status/wait/cancel/resume call makes first.
//!
//! Split out of [`super`] because both are policies the MCP surface depends on
//! and neither belongs to the run lifecycle itself: the wait never blocks
//! forever and never pretends a run finished, and the owner check is the only
//! thing standing between a session and another session's run.

use std::time::Duration;

use super::super::store::owner::run_is_owned_by;
use super::super::store::{TERMINAL_STATUSES, WorkRun};
use super::RunManager;

/// Poll interval while waiting for a run to settle.
const WAIT_POLL: Duration = Duration::from_millis(50);

/// The single refusal every ownership failure shares, so an unknown run, a run
/// with no owner row, and a run owned by another session are indistinguishable
/// from the outside.
const NOT_AVAILABLE: &str = "explorer run is not available to this session";

impl RunManager {
    /// Load a run only for the session that owns it.
    ///
    /// The owner binding is server-side and exact. The message never names the
    /// session, the run's owner, or the worktree, so a caller cannot use the
    /// error to discover them either.
    pub fn owned_run(&self, run_id: &str, session: &str) -> Result<WorkRun, String> {
        let run = self.load(run_id)?.ok_or_else(|| NOT_AVAILABLE.to_owned())?;
        if !run_is_owned_by(&self.connection(), run_id, session)? {
            return Err(NOT_AVAILABLE.to_owned());
        }
        Ok(run)
    }

    /// Wait up to `budget` for a run to reach a terminal status, then return
    /// the row as it stands.
    ///
    /// The wait is a bounded poll, not a join: it returns the current row when
    /// the budget runs out, so a caller always learns whether the run finished
    /// instead of hanging on a turn that may take minutes. Ownership is
    /// checked before the first poll and again on every read, so a caller that
    /// lost the run mid-wait is refused rather than handed the result.
    pub async fn wait_run(
        &self,
        run_id: &str,
        session: &str,
        budget: Duration,
    ) -> Result<WorkRun, String> {
        self.owned_run(run_id, session)?;
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let run = self.owned_run(run_id, session)?;
            if Self::is_terminal(&run) || tokio::time::Instant::now() >= deadline {
                return Ok(run);
            }
            tokio::time::sleep(WAIT_POLL).await;
        }
    }

    /// Whether a run row is terminal. Exposed so the MCP surface can label a
    /// bounded wait's result without duplicating the status vocabulary.
    pub fn is_terminal(run: &WorkRun) -> bool {
        TERMINAL_STATUSES.contains(&run.status.as_str())
    }

    /// Cancel a run, but only for the session that owns it.
    ///
    /// This is the cancellation entry point the delegation surface uses. The
    /// ownership check happens before the escalation ladder starts, so a
    /// foreign session cannot record cancel intent on another session's run,
    /// cannot reach its live ACP session, and cannot abort its task.
    pub async fn cancel_owned_run(&self, run_id: &str, session: &str) -> Result<WorkRun, String> {
        self.owned_run(run_id, session)?;
        self.cancel_run(run_id).await
    }

    /// Resume a run, but only for the session that owns it.
    ///
    /// Ownership is resolved before the durable row is reopened, so a foreign
    /// session cannot turn another session's `interrupted` run into a live
    /// process — and the reopened row keeps the original binding, so the
    /// resumed run is still owned by the same session.
    pub async fn resume_owned_run(
        &self,
        run_id: &str,
        session: &str,
        follow_up: crate::mcp::agent::types::ExplorerPrompt,
    ) -> Result<WorkRun, String> {
        self.owned_run(run_id, session)?;
        self.resume_run(run_id, follow_up)
            .await
            .map_err(|error| error.message)
    }
}
