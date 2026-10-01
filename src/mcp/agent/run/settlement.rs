//! The single guarantee that a launched run leaves one terminal record.
//!
//! A run task can stop three ways: it finishes, it panics, or a cancel
//! escalation aborts it. Only the first one ran the terminal write on
//! its own, so a panic or an abort left a `pending`/`running` row and a
//! registered handle behind — a run nothing could cancel, nothing could
//! resume, and nothing would ever recover until the process restarted.
//!
//! This guard closes that. A task future is unwound on a panic and
//! dropped on an abort, so the guard's `Drop` runs on all three exits
//! and the settle happens exactly once: the row is made terminal, the
//! registration is retired.

use std::sync::Arc;

use super::super::store::{
    RUN_CANCELLED, RUN_INTERRUPTED, TERMINAL_STATUSES, WorkRunUpdate, now_epoch_seconds,
};
use super::RunManager;
use super::flight::Flight;

impl RunManager {
    /// Guarantee the durable row for `run_id` is terminal, if it is not
    /// already. An already-terminal row is left alone: the terminal
    /// freeze is the record of what happened, and a caller that lost a
    /// race to a natural completion must be able to read that
    /// completion back.
    ///
    /// `cancelled` picks the status a forced settle reports. It is a
    /// best effort, not a promise: the store refuses a transition out
    /// of a terminal status, so a run that finished first keeps its own
    /// outcome.
    pub(crate) fn ensure_terminal(&self, run_id: &str, cancelled: bool) {
        let Ok(Some(run)) = self.load(run_id) else {
            return;
        };
        if TERMINAL_STATUSES.contains(&run.status.as_str()) {
            return;
        }
        let (status, error) = if cancelled {
            (
                RUN_CANCELLED,
                "cancelled before the research turn completed",
            )
        } else {
            (
                RUN_INTERRUPTED,
                "the research run task ended without recording an outcome",
            )
        };
        let _ = self.persist(
            run_id,
            &WorkRunUpdate {
                status: Some(status.to_owned()),
                error: Some(error.to_owned()),
                finished_at: Some(now_epoch_seconds()),
                ..WorkRunUpdate::default()
            },
        );
    }
}

/// Settles one launched run when its task stops, however it stops.
pub(crate) struct Settlement {
    manager: RunManager,
    run_id: String,
    flight: Arc<Flight>,
}

impl Settlement {
    pub(crate) fn new(manager: &RunManager, run_id: &str, flight: Arc<Flight>) -> Self {
        Self {
            manager: manager.clone(),
            run_id: run_id.to_owned(),
            flight,
        }
    }
}

impl Drop for Settlement {
    fn drop(&mut self) {
        // Read before the registration is retired: it is where the
        // cancel intent lives.
        let cancelled = self.flight.is_cancelled();
        self.flight.mark_done();
        self.manager.ensure_terminal(&self.run_id, cancelled);
        self.manager.flights.forget(&self.run_id, &self.flight);
        // Every terminal settlement removes the run's private scratch
        // directory. `remove` refuses any path outside the server-owned
        // root, so a corrupted row cannot turn this into an arbitrary
        // recursive delete.
        if let Ok(Some(run)) = self.manager.load(&self.run_id) {
            super::super::scratch::remove(std::path::Path::new(&run.scratch_cwd));
        }
    }
}
