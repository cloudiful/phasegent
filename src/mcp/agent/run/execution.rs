//! The run task body: spawn or reconnect, negotiate, prompt, and land
//! a terminal status.
//!
//! Split out of [`super`] because it is the only place that holds an
//! `await` across the run's whole life, and it needs the manager's
//! storage and registry to be reachable from one place.
//!
//! Cancel intent is checked at every stage rather than only while a
//! prompt is in flight, so a run cancelled during the spawn or the
//! negotiation stops instead of racing on. The ACP process is killed
//! on every exit, including the error paths, and `kill_on_drop` on the
//! spawn covers an abort that lands mid-handshake. Whatever ends the
//! task, the run's row is made terminal and its registration retired
//! by the settlement guard in [`super::settlement`].

use std::sync::Arc;
use std::time::Duration;

use super::super::error::{AgentError, AgentErrorKind};
use super::super::session::AcpSession;
use super::super::store::{RUN_RUNNING, WorkRunUpdate};
use super::super::types::{AcpSpawnConfig, ExplorerPrompt, PromptOutcome};
use super::RunManager;
use super::flight::Flight;

/// How long a cancelled turn is given to flush the agent's remaining
/// chunks before the process is killed.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

impl RunManager {
    /// Execute one run to a terminal status, persisting transitions.
    /// Each database call takes the storage lock for that call only; no
    /// guard is held across an `await`.
    ///
    /// Only the normal exit settles the row here. The launch task's
    /// settlement guard covers the other two ways a task can stop, so a
    /// panic or an abort cannot leave a run open.
    pub(crate) async fn execute_run(
        &self,
        run_id: &str,
        config: AcpSpawnConfig,
        prompt: ExplorerPrompt,
        resume: Option<String>,
        flight: &Arc<Flight>,
    ) {
        let outcome = self
            .drive(run_id, &config, &prompt, resume.as_deref(), flight)
            .await;
        let cancelled = flight.is_cancelled();
        super::cancel::finish_run(self, run_id, outcome, cancelled).await;
    }

    async fn drive(
        &self,
        run_id: &str,
        config: &AcpSpawnConfig,
        prompt: &ExplorerPrompt,
        resume: Option<&str>,
        flight: &Arc<Flight>,
    ) -> Result<PromptOutcome, AgentError> {
        if flight.is_cancelled() {
            return Err(AgentError::cancelled());
        }
        self.persist(
            run_id,
            &WorkRunUpdate {
                status: Some(RUN_RUNNING.to_owned()),
                ..WorkRunUpdate::default()
            },
        )
        .map_err(Self::storage_error)?;
        if flight.is_cancelled() {
            return Err(AgentError::cancelled());
        }
        let session = Arc::new(match resume {
            Some(session_id) => AcpSession::resume(config, session_id).await,
            None => AcpSession::start(config).await,
        }?);
        if flight.is_cancelled() {
            session.kill().await;
            return Err(AgentError::cancelled());
        }
        flight.attach_session(session.clone());
        let acp_session_id = session.session_id().await;
        self.persist(
            run_id,
            &WorkRunUpdate {
                acp_session_id: Some(acp_session_id),
                ..WorkRunUpdate::default()
            },
        )
        .map_err(Self::storage_error)?;
        if flight.is_cancelled() {
            session.kill().await;
            return Err(AgentError::cancelled());
        }
        session.negotiate_explorer().await?;
        if flight.is_cancelled() {
            session.kill().await;
            return Err(AgentError::cancelled());
        }
        match session.prompt(prompt).await {
            Ok(outcome) => {
                session.kill().await;
                Ok(outcome)
            }
            Err(error) => {
                if matches!(
                    error.kind,
                    AgentErrorKind::Cancelled | AgentErrorKind::Timeout
                ) || flight.is_cancelled()
                {
                    // Let the agent flush remaining chunks before the
                    // process dies, then drain the partial transcript.
                    let _ = tokio::time::timeout(DRAIN_GRACE, session.cancel()).await;
                    let (text, truncated) = session.take_transcript().await;
                    if !text.is_empty() {
                        // Best effort: the terminal update below still
                        // records the run's outcome.
                        let _ = self.persist(
                            run_id,
                            &WorkRunUpdate {
                                output: Some(text),
                                output_truncated: Some(truncated),
                                ..WorkRunUpdate::default()
                            },
                        );
                    }
                }
                session.kill().await;
                Err(error)
            }
        }
    }
}
