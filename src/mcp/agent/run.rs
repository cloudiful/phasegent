//! In-process run manager over the durable run store.
//!
//! The manager keeps active runs in memory (a launch task handle, then
//! the live [`AcpSession`]) and persists every transition.
//!
//! ## The ledger has exactly one owner
//!
//! [`RunManager::new`] first claims the database with a kernel-held exclusive
//! lock ([`process_lock`]) and *then* runs [`RunManager::recover`]. Recovery
//! means "every durable `pending`/`running` row with no in-process handle
//! belongs to a dead process, so mark it `interrupted`", which is only a sound
//! judgement for the process that owns the ledger — a second process would
//! read the first one's live row, find no local flight for it, and mark it
//! interrupted. Holding the lock from construction to drop makes the two
//! distinct: a competing process is refused at construction and never reaches
//! recovery, and the kernel releases the lock when the owner exits, so a
//! successor recovers exactly the rows its predecessor really did leave
//! behind. That successor is what makes an interrupted run resumable: the row
//! keeps its ACP session id, so [`RunManager::resume_run`] can continue it in a
//! fresh process through `session/load`.
//!
//! The lock also covers spawning: a manager that could not claim the ledger has
//! no methods to call, so "refuse rather than spawn a second ACP process for
//! one session" needs no separate check.
//!
//! `Storage` wraps a non-`Sync` `rusqlite::Connection`, so it lives
//! behind a `std::sync::Mutex` and every database call takes the lock
//! for the duration of that call only; no guard is ever held across an
//! `await`.

use std::sync::{Arc, Mutex};

use super::error::{AgentError, AgentErrorKind, AgentResult};
use super::session::AcpSession;
use super::store::{WorkRun, WorkRunUpdate, now_epoch_seconds};
use super::types::{AcpSpawnConfig, ResearchPrompt};
use crate::infra::storage::Storage;

mod cancel;
mod execution;
mod flight;
mod process_lock;
mod settlement;
mod shared;
mod wait;

#[cfg(test)]
mod flight_tests;

#[cfg(test)]
mod process_lock_tests;

use process_lock::ProcessLock;

#[allow(unused_imports)]
pub use cancel::{CANCEL_GRACE, CANCEL_SETTLE};
#[allow(unused_imports)]
pub use shared::shared;

/// In-process run manager, and the sole owner of one database's run ledger.
#[derive(Clone)]
pub struct RunManager {
    pub(crate) storage: Arc<Mutex<Storage>>,
    pub(crate) flights: flight::FlightRegistry,
    /// The held ledger lock, kept for this manager's whole lifetime. Every
    /// clone shares this handle, so the lock is released when the last clone
    /// (or the process) goes away — never by a clone being dropped.
    ledger: Arc<ProcessLock>,
}

impl RunManager {
    /// Claim `storage`'s run ledger and mark its orphaned durable runs
    /// interrupted.
    ///
    /// Fails when another live process owns the ledger. That failure is the
    /// whole point: it happens before recovery, so a competing process cannot
    /// even read the rows it would otherwise rewrite, and no ACP process is
    /// spawned.
    pub fn new(storage: Storage) -> Result<Self, String> {
        let ledger = ProcessLock::acquire(&storage.path).map_err(|error| error.message())?;
        let manager = Self {
            storage: Arc::new(Mutex::new(storage)),
            flights: flight::FlightRegistry::default(),
            ledger: Arc::new(ledger),
        };
        manager.recover()?;
        Ok(manager)
    }

    pub(crate) fn storage_error(message: String) -> AgentError {
        AgentError::new(AgentErrorKind::Protocol, message)
    }

    /// The storage connection, tolerating a poisoned lock. A run task
    /// that panicked mid-write must not make every later run
    /// unwrap-fail, and the settlement that runs during that unwind
    /// needs the connection more than it needs the panic back.
    pub(crate) fn connection(&self) -> std::sync::MutexGuard<'_, Storage> {
        self.storage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Persist one run update on the manager's connection.
    pub(crate) fn persist(&self, run_id: &str, update: &WorkRunUpdate) -> Result<WorkRun, String> {
        self.connection().update_work_run(run_id, update)
    }

    pub(crate) fn load(&self, run_id: &str) -> Result<Option<WorkRun>, String> {
        self.connection().load_work_run(run_id)
    }

    /// Mark every non-terminal run without an in-process handle as
    /// interrupted, and clean the scratch directory its dead process left
    /// behind. Called on construction; safe to call again.
    pub fn recover(&self) -> Result<usize, String> {
        let open = self.connection().list_work_runs(true, 1_000)?;
        let mut repaired = 0;
        for run in open {
            if self.flights.contains(&run.run_id) {
                continue;
            }
            self.connection()
                .update_work_run(
                    &run.run_id,
                    &WorkRunUpdate {
                        status: Some(super::store::RUN_INTERRUPTED.to_owned()),
                        error: Some(
                            "phasegent process restarted while the run was active".to_owned(),
                        ),
                        finished_at: Some(now_epoch_seconds()),
                        ..WorkRunUpdate::default()
                    },
                )
                .map_err(|message| format!("could not recover run {}: {message}", run.run_id))?;
            // The owning process is gone, so nothing holds this scratch
            // directory: remove it rather than leak it. `remove` refuses any
            // path outside the server-owned root.
            super::scratch::remove(std::path::Path::new(&run.scratch_cwd));
            repaired += 1;
        }
        Ok(repaired)
    }

    /// Whether a run currently has a live process or launch task.
    pub fn is_in_flight(&self, run_id: &str) -> bool {
        self.flights.contains(run_id)
    }

    /// Start one research run: persist the row, register it, spawn the
    /// process, negotiate, and run the prompt on a background task.
    /// Returns as soon as the row is durable and the run is registered;
    /// completion lands in the store.
    ///
    /// `owner_session` is the host-bound session the caller resolved. The
    /// binding is written with the run, so a refused registration leaves no
    /// owner row and a run whose registration is later refused is unusable
    /// rather than claimable.
    pub async fn start_run(
        &self,
        run_id: &str,
        owner_session: &str,
        config: AcpSpawnConfig,
        prompt: ResearchPrompt,
    ) -> AgentResult<WorkRun> {
        // The run owns a fresh, server-created scratch directory: the caller
        // never names the cwd, and no repository or worktree path is ever
        // placed on the process.
        let scratch = super::scratch::create(run_id).map_err(Self::storage_error)?;
        let config = config.with_cwd(scratch.clone());
        // The registration lands before the spawn so a worker that
        // polls the task first can still find its own entry.
        let flight = match self.flights.register(run_id) {
            Ok(flight) => flight,
            Err(error) => {
                super::scratch::remove(&scratch);
                return Err(Self::storage_error(error));
            }
        };
        let created = match self.register_owned(run_id, owner_session, &config, &prompt) {
            Ok(created) => created,
            Err(error) => {
                // The insert is this call's own row, so a refusal must
                // not leave a registration nothing will ever settle.
                self.flights.forget(run_id, &flight);
                super::scratch::remove(&scratch);
                return Err(Self::storage_error(error));
            }
        };
        self.launch(run_id.to_owned(), config, prompt, None, flight);
        Ok(created)
    }

    /// Write the run row and its owner binding together.
    ///
    /// Both writes happen while this call holds the storage lock, so the
    /// durable pair is either complete or absent: a run that exists without an
    /// owner is never observable, and a rejected owner never leaves a run row
    /// that looks claimable. The owner is bound before the process is spawned,
    /// so the very first poll of the run already resolves an owner.
    fn register_owned(
        &self,
        run_id: &str,
        owner_session: &str,
        config: &AcpSpawnConfig,
        prompt: &ResearchPrompt,
    ) -> Result<WorkRun, String> {
        let storage = self.connection();
        let created =
            storage.create_work_run(run_id, &config.cwd.display().to_string(), &prompt.text)?;
        storage.bind_run_owner(run_id, owner_session)?;
        Ok(created)
    }

    /// Continue an interrupted run's ACP session in a fresh process and a
    /// fresh scratch directory. The durable row keeps its id, prompt, partial
    /// output, and persisted ACP session id; only its terminal markers are
    /// cleared and its scratch cwd replaced, and the new process reconnects
    /// with `session/load` so the agent still holds the conversation.
    ///
    /// The caller must already have resolved ownership through
    /// [`Self::resume_owned_run`], which is the entry point the delegation
    /// surface uses; this is the raw lifecycle step underneath it.
    pub(crate) async fn resume_run(
        &self,
        run_id: &str,
        follow_up: ResearchPrompt,
    ) -> AgentResult<WorkRun> {
        // The registration is the gate, and it is taken before the store
        // is touched. A second resume that lost this race used to reopen
        // the live run's row underneath it, and a cancel arriving in the
        // gap had no registration to record its intent on.
        let flight = self.flights.register(run_id).map_err(Self::storage_error)?;
        let (reopened, session_id) = match self.reopen_for_resume(run_id) {
            Ok(pair) => pair,
            Err(error) => {
                self.flights.forget(run_id, &flight);
                return Err(error);
            }
        };
        let config = AcpSpawnConfig::new(std::path::PathBuf::from(&reopened.scratch_cwd));
        self.launch(
            run_id.to_owned(),
            config,
            follow_up,
            Some(session_id),
            flight,
        );
        Ok(reopened)
    }

    /// Validate and reopen the durable row for a resume in a fresh scratch
    /// directory. Kept apart from [`Self::resume_run`] so every refusal is one
    /// early return, and the caller can retire the registration it took.
    fn reopen_for_resume(&self, run_id: &str) -> Result<(WorkRun, String), AgentError> {
        let existing = self
            .load(run_id)
            .map_err(Self::storage_error)?
            .ok_or_else(|| Self::storage_error(format!("research run '{run_id}' was not found")))?;
        let session_id = existing.acp_session_id.clone().ok_or_else(|| {
            Self::storage_error(format!(
                "research run '{run_id}' has no ACP session to resume"
            ))
        })?;
        // A resume runs in a fresh scratch directory; the previous one is
        // already gone (terminal settlement removed it) or is an orphan this
        // process no longer holds.
        let scratch = super::scratch::create(run_id).map_err(Self::storage_error)?;
        let reopened = match self
            .connection()
            .resume_work_run(run_id, &scratch.display().to_string())
            .map_err(Self::storage_error)
        {
            Ok(reopened) => reopened,
            Err(error) => {
                super::scratch::remove(&scratch);
                return Err(error);
            }
        };
        super::scratch::remove(std::path::Path::new(&existing.scratch_cwd));
        Ok((reopened, session_id))
    }

    /// Register the launch task and let it drive the run to a terminal
    /// status. The abort handle is published immediately so a cancel
    /// that lands while the process is spawning can escalate to an
    /// abort instead of waiting for the handshake.
    ///
    /// The settlement guard moves into the task, so the run is made
    /// terminal and its registration retired however the task stops:
    /// finished, panicked, or aborted.
    fn launch(
        &self,
        run_id: String,
        config: AcpSpawnConfig,
        prompt: ResearchPrompt,
        resume: Option<String>,
        flight: Arc<flight::Flight>,
    ) {
        let manager = self.clone();
        let settle = settlement::Settlement::new(&manager, &run_id, Arc::clone(&flight));
        let owned = Arc::clone(&flight);
        let handle = tokio::spawn(async move {
            let _settle = settle;
            manager
                .execute_run(&run_id, config, prompt, resume, &owned)
                .await;
        })
        .abort_handle();
        flight.attach_abort(handle);
    }

    /// Snapshot the live session, if one is attached, for wait/status
    /// callers.
    pub fn live_session(&self, run_id: &str) -> Option<Arc<AcpSession>> {
        let flight = self.flights.get(run_id)?;
        flight.live_session()
    }

    /// Whether a cancel was requested for this run, whether or not the
    /// process is up yet.
    pub fn is_cancelled(&self, run_id: &str) -> bool {
        self.flights.is_cancelled(run_id)
    }

    /// Whether two handles address the same ledger, used by the shared-manager
    /// test to prove one database resolves to one manager.
    #[cfg(test)]
    pub(crate) fn same_manager_as(&self, other: &RunManager) -> bool {
        Arc::ptr_eq(&self.storage, &other.storage)
    }

    /// The lock file this manager holds, used by the process-lock tests to
    /// prove the lock is taken beside the database and retained for the
    /// manager's lifetime.
    #[cfg(test)]
    pub(crate) fn ledger_lock_path(&self) -> &std::path::Path {
        self.ledger.path()
    }

    /// Whether two handles share one held lock, so a clone of a manager can
    /// never be mistaken for a second claim on the ledger.
    #[cfg(test)]
    pub(crate) fn shares_ledger_lock_with(&self, other: &RunManager) -> bool {
        Arc::ptr_eq(&self.ledger, &other.ledger)
    }
}
