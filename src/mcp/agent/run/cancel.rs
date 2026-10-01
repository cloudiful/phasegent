//! Cancellation and the terminal write.
//!
//! Split out of [`super`] because every wait here is bounded and the
//! bound is the point: a cancel must never block on the run it is
//! cancelling. The escalation ladder is notify → settle → force
//! teardown → record, and each step has its own budget.
//!
//! Every branch ends at the same place: the run's durable row is
//! terminal and its registration retired. A cancel that finds nothing
//! registered in this process still records the cancel durably, so a
//! `running` row can never outlive the process that owned it.

use std::time::Duration;

use super::super::error::{AgentErrorKind, AgentResult};
use super::super::store::{
    RUN_CANCELLED, RUN_COMPLETED, RUN_FAILED, RUN_TIMED_OUT, WorkRun, WorkRunUpdate,
    now_epoch_seconds,
};
use super::super::types::{PromptOutcome, StopReason};
use super::RunManager;
use super::flight::CancelLever;

/// How long a cancel waits for the agent to acknowledge
/// `session/cancel` before escalating to a process kill.
pub const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// How long a cancel waits for the run task to reach a terminal status
/// after the notification, before forcing teardown.
pub const CANCEL_SETTLE: Duration = Duration::from_secs(10);
/// Poll interval while waiting for a run to settle.
const SETTLE_POLL: Duration = Duration::from_millis(20);

impl RunManager {
    /// Cancel one active run. A live ACP session receives the cancel
    /// notification and the run task persists the cancelled status; a
    /// still-launching run observes the intent at its next stage check;
    /// a run that ignores both is stopped by aborting its task. Nothing
    /// waits on the in-flight turn itself, and every path is bounded, so
    /// a cancel can never be blocked by the run it is cancelling.
    ///
    /// The caller must already have resolved ownership through
    /// [`Self::cancel_owned_run`], which is the entry point the delegation
    /// surface uses; this is the raw lifecycle step underneath it.
    pub(crate) async fn cancel_run(&self, run_id: &str) -> Result<WorkRun, String> {
        if self.load(run_id)?.is_none() {
            return Err(format!("research run '{run_id}' was not found"));
        }
        match self.flights.request_cancel(run_id) {
            CancelLever::Absent => {
                // Nothing is running here, so nothing else will settle
                // this row. Recording the cancel is what keeps a
                // `running` row from surviving with no live process.
                self.ensure_terminal(run_id, true);
            }
            CancelLever::Launching => {
                // The launch task reads the recorded intent at every
                // stage and settles the run itself; the abort is only
                // the fallback for a task that does not.
                if !self.settle(run_id, CANCEL_GRACE).await {
                    self.escalate_to_abort(run_id).await;
                }
            }
            CancelLever::Live(session) => {
                let _ = tokio::time::timeout(CANCEL_GRACE, session.cancel()).await;
                if !self.settle(run_id, CANCEL_SETTLE).await {
                    session.kill().await;
                    if !self.settle(run_id, CANCEL_GRACE).await {
                        self.escalate_to_abort(run_id).await;
                    }
                }
            }
        }
        // Return the row as it stands now, not the pre-cancel
        // snapshot: a cancel that lost the race to a natural completion
        // must report that completion, not fail on the terminal freeze.
        self.load(run_id)?
            .ok_or_else(|| format!("research run '{run_id}' was not found"))
    }

    /// Stop the launch task, then make sure the row is terminal even if
    /// the task was already gone. Bounded, because a wedged task must
    /// not hold a cancel open.
    async fn escalate_to_abort(&self, run_id: &str) {
        self.flights.abort(run_id);
        if !self.settle(run_id, CANCEL_GRACE).await {
            self.ensure_terminal(run_id, true);
        }
    }

    /// Whether the run left the registry within `budget`.
    async fn settle(&self, run_id: &str, budget: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            if !self.flights.contains(run_id) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }
}

/// Persist a run's terminal status, keeping the partial transcript a
/// cancelled or timed-out turn produced.
pub(crate) async fn finish_run(
    manager: &RunManager,
    run_id: &str,
    outcome: AgentResult<PromptOutcome>,
    cancelled: bool,
) {
    let update = match outcome {
        Ok(outcome) => {
            let completed = matches!(outcome.stop_reason, StopReason::EndTurn | StopReason::Other);
            // A cancel the caller asked for wins over a turn that
            // happened to end at the same moment: reporting
            // `completed` for work the caller stopped would mislead.
            let status = if cancelled || !completed {
                RUN_CANCELLED
            } else {
                RUN_COMPLETED
            };
            WorkRunUpdate {
                status: Some(status.to_owned()),
                output: Some(outcome.text),
                output_truncated: Some(outcome.truncated),
                finished_at: Some(now_epoch_seconds()),
                ..WorkRunUpdate::default()
            }
        }
        Err(error) => {
            let (status, message) = match error.kind {
                AgentErrorKind::Cancelled => (RUN_CANCELLED, error.message),
                AgentErrorKind::Timeout => (RUN_TIMED_OUT, error.message),
                _ => (RUN_FAILED, error.message),
            };
            let status = if cancelled { RUN_CANCELLED } else { status };
            WorkRunUpdate {
                status: Some(status.to_owned()),
                error: Some(message),
                finished_at: Some(now_epoch_seconds()),
                ..WorkRunUpdate::default()
            }
        }
    };
    if let Err(message) = manager.persist(run_id, &update) {
        // A cancel that had to force the run already wrote the terminal
        // row; the store's terminal freeze refuses this write, and that
        // is the expected outcome rather than a failure to report.
        if manager.load(run_id).ok().flatten().is_some_and(|run| {
            super::super::store::TERMINAL_STATUSES.contains(&run.status.as_str())
        }) {
            return;
        }
        // The crate's JSON error envelope for out-of-band failures; the
        // run row itself stays the record of the outcome.
        eprintln!(
            "{}",
            serde_json::json!({"error":{"kind":"research_run","operation": run_id,"message": message}})
        );
    }
}
