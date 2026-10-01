//! In-process registry of active explorer runs.
//!
//! One entry per active run, keyed by run id, holding the two things
//! cancellation needs: the launch task's abort handle, and once the
//! process is up, the live [`AcpSession`]. The entry is inserted
//! *before* the launch task is spawned, so a worker that polls the task
//! first can still find its own registration — the ordering this
//! registry exists to guarantee.
//!
//! Each entry carries its cancel intent. A cancel that arrives before
//! the process exists is recorded rather than lost, and the launch
//! task reads it at every stage, so a cancelled run stops without
//! relying on a `JoinHandle` race.
//!
//! The abort handle is tracked apart from the process state on purpose:
//! publishing the live session used to overwrite it, which left a run
//! whose process was up with no lever a cancel could escalate to.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::super::session::AcpSession;

/// What a registered run is doing right now.
#[derive(Default)]
enum State {
    /// Registered, or the launch task is spawning/negotiating. The
    /// abort handle is published right after the spawn and survives the
    /// session going live, so this state is always escapable.
    #[default]
    Launching,
    /// The ACP session is live and cancellable on the wire.
    Live(Arc<AcpSession>),
    /// The run task finished; the entry is about to be removed.
    Done,
}

/// One registered run.
#[derive(Default)]
pub(crate) struct Flight {
    state: Mutex<State>,
    abort: Mutex<Option<tokio::task::AbortHandle>>,
    cancel: AtomicBool,
}

/// What a caller should do to cancel a registered run.
pub(crate) enum CancelLever {
    /// Nothing is registered: the run is not active in this process.
    Absent,
    /// Registered, still spawning or negotiating. The launch task reads
    /// the recorded intent at every stage and settles the run itself.
    Launching,
    /// The session is live; notify the agent, then escalate.
    Live(Arc<AcpSession>),
}

impl Flight {
    /// Record cancel intent. Always succeeds, so a cancel that lands
    /// before the process exists is never dropped.
    pub(crate) fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Publish the launch task's abort handle. A run that already
    /// finished keeps it inert, so a late handle cannot resurrect a
    /// dead entry.
    pub(crate) fn attach_abort(&self, handle: tokio::task::AbortHandle) {
        // The state lock is released before the abort lock is taken, in
        // the same order as `abort`, so the two can never deadlock.
        if self.finished() {
            return;
        }
        *self.abort.lock().expect("flight abort lock") = Some(handle);
    }

    fn finished(&self) -> bool {
        matches!(*self.state.lock().expect("flight state lock"), State::Done)
    }

    /// Publish the live session.
    pub(crate) fn attach_session(&self, session: Arc<AcpSession>) {
        let mut state = self.state.lock().expect("flight state lock");
        if !matches!(*state, State::Done) {
            *state = State::Live(session);
        }
    }

    pub(crate) fn mark_done(&self) {
        *self.state.lock().expect("flight state lock") = State::Done;
    }

    /// Stop the launch task, if it is still running. Idempotent: the
    /// handle is taken, so a second escalation cannot abort whatever
    /// registered under the same id afterwards.
    pub(crate) fn abort(&self) -> bool {
        if self.finished() {
            return false;
        }
        match self.abort.lock().expect("flight abort lock").take() {
            Some(handle) => {
                handle.abort();
                true
            }
            None => false,
        }
    }

    /// The lever a cancel should pull right now.
    fn cancel_lever(&self) -> CancelLever {
        match &*self.state.lock().expect("flight state lock") {
            State::Live(session) => CancelLever::Live(session.clone()),
            State::Launching => CancelLever::Launching,
            State::Done => CancelLever::Absent,
        }
    }

    /// The live session, if the process is up.
    pub(crate) fn live_session(&self) -> Option<Arc<AcpSession>> {
        match &*self.state.lock().expect("flight state lock") {
            State::Live(session) => Some(session.clone()),
            _ => None,
        }
    }
}

/// Every active run in this process.
#[derive(Clone, Default)]
pub(crate) struct FlightRegistry {
    entries: Arc<Mutex<HashMap<String, Arc<Flight>>>>,
}

impl FlightRegistry {
    /// Register a run before its task is spawned. Fails when the id is
    /// already active, so a second start can never race the first.
    pub(crate) fn register(&self, run_id: &str) -> Result<Arc<Flight>, String> {
        let mut entries = self.entries.lock().expect("flight registry lock");
        if entries.contains_key(run_id) {
            return Err(format!("explorer run '{run_id}' is already active"));
        }
        let flight = Arc::new(Flight::default());
        entries.insert(run_id.to_owned(), flight.clone());
        Ok(flight)
    }

    pub(crate) fn get(&self, run_id: &str) -> Option<Arc<Flight>> {
        self.entries
            .lock()
            .expect("flight registry lock")
            .get(run_id)
            .cloned()
    }

    pub(crate) fn contains(&self, run_id: &str) -> bool {
        self.entries
            .lock()
            .expect("flight registry lock")
            .contains_key(run_id)
    }

    /// Remove an entry, but only if it is still the same registration.
    /// A run id is never reused while active, and a stale remover can
    /// therefore not delete a newer registration's entry.
    pub(crate) fn forget(&self, run_id: &str, flight: &Arc<Flight>) {
        let mut entries = self.entries.lock().expect("flight registry lock");
        if entries
            .get(run_id)
            .is_some_and(|current| Arc::ptr_eq(current, flight))
        {
            entries.remove(run_id);
        }
    }

    /// Record cancel intent and return the current lever.
    pub(crate) fn request_cancel(&self, run_id: &str) -> CancelLever {
        let Some(flight) = self.get(run_id) else {
            return CancelLever::Absent;
        };
        flight.request_cancel();
        flight.cancel_lever()
    }

    /// Stop the launch task for an active run. A run with no entry, or
    /// one that already finished, is not abortable.
    pub(crate) fn abort(&self, run_id: &str) -> bool {
        self.get(run_id).is_some_and(|flight| flight.abort())
    }

    pub(crate) fn is_cancelled(&self, run_id: &str) -> bool {
        self.get(run_id).is_some_and(|flight| flight.is_cancelled())
    }
}
