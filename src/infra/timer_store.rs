use crate::infra::storage::Storage;
use crate::infra::timer_ledger::{
    TIMER_STATUS_RUNNING, TimerRun, TimerRunOwner, TimerStatusFilter, ensure_same_timer_identity,
    timer_run_from_row, validate_owner_field, validate_timer_identity,
};
use rusqlite::{OptionalExtension, params};

impl Storage {
    /// Load one execution-ledger row by its caller-supplied run id.
    pub fn load_timer_run(&self, run_id: &str) -> Result<Option<TimerRun>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT run_id, issue_id, phase, role, attempt, started_at, finished_at, status, \
                        elapsed_seconds, rounded_hours, activity_id, redmine_time_entry_id, \
                        sync_status, sync_error, owner_session_id, owner_call_id, \
                        projection_token, projection_claimed_at \
                 FROM execution_timer_runs WHERE run_id = ?1",
            )
            .map_err(|error| format!("could not prepare timer run load: {error}"))?;
        statement
            .query_row(params![run_id], timer_run_from_row)
            .optional()
            .map_err(|error| format!("could not read timer run: {error}"))
    }

    /// List execution-ledger rows filtered by `status`. The result is
    /// ordered newest-started-first so callers can scan the open orphans
    /// without a separate query. Secrets are never part of the projection.
    pub fn list_timer_runs(
        &self,
        status: TimerStatusFilter,
        limit: u32,
    ) -> Result<Vec<TimerRun>, String> {
        // Clamp the cap to keep the bounded JSON output small and to
        // stop a runaway listing from spending its time on rows nobody
        // asked for. The default 100 rows keeps the recovery workflow
        // observable without paging logic.
        let clamped = limit.clamp(1, 1_000);
        let sql = match status {
            TimerStatusFilter::Running => {
                "SELECT run_id, issue_id, phase, role, attempt, started_at, finished_at, status, \
                        elapsed_seconds, rounded_hours, activity_id, redmine_time_entry_id, \
                        sync_status, sync_error, owner_session_id, owner_call_id, \
                        projection_token, projection_claimed_at \
                 FROM execution_timer_runs \
                 WHERE status = 'running' \
                 ORDER BY started_at DESC LIMIT ?1"
            }
            TimerStatusFilter::Finished => {
                "SELECT run_id, issue_id, phase, role, attempt, started_at, finished_at, status, \
                        elapsed_seconds, rounded_hours, activity_id, redmine_time_entry_id, \
                        sync_status, sync_error, owner_session_id, owner_call_id, \
                        projection_token, projection_claimed_at \
                 FROM execution_timer_runs \
                 WHERE status <> 'running' \
                 ORDER BY started_at DESC LIMIT ?1"
            }
            TimerStatusFilter::All => {
                "SELECT run_id, issue_id, phase, role, attempt, started_at, finished_at, status, \
                        elapsed_seconds, rounded_hours, activity_id, redmine_time_entry_id, \
                        sync_status, sync_error, owner_session_id, owner_call_id, \
                        projection_token, projection_claimed_at \
                 FROM execution_timer_runs \
                 ORDER BY started_at DESC LIMIT ?1"
            }
        };
        let mut statement = self
            .connection
            .prepare(sql)
            .map_err(|error| format!("could not prepare timer run list: {error}"))?;
        let rows = statement
            .query_map(params![clamped as i64], timer_run_from_row)
            .map_err(|error| format!("could not read timer run list: {error}"))?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row.map_err(|error| format!("could not decode timer row: {error}"))?);
        }
        Ok(runs)
    }

    /// Persist the start of one wall-clock run. Repeating the same run id and
    /// identity is a no-op; a different identity or an already-finished run is
    /// rejected before any remote operation is attempted. The legacy
    /// six-argument signature preserves backward compatibility with the
    /// Phase 5A callers; new code should prefer
    /// [`start_timer_run_with_owner`] when owner metadata is available.
    #[allow(dead_code)]
    pub fn start_timer_run(
        &self,
        run_id: &str,
        issue: u64,
        phase: &str,
        role: &str,
        attempt: u64,
        started_at: i64,
    ) -> Result<TimerRun, String> {
        self.start_timer_run_with_owner(
            run_id,
            issue,
            phase,
            role,
            attempt,
            started_at,
            &TimerRunOwner::default(),
        )
    }

    /// Persist the start of one wall-clock run and optionally record the
    /// OpenCode session / call identifiers that own it. The owner columns
    /// are nullable so older callers and migrations leave them null
    /// without breaking the row shape.
    #[allow(clippy::too_many_arguments)]
    pub fn start_timer_run_with_owner(
        &self,
        run_id: &str,
        issue: u64,
        phase: &str,
        role: &str,
        attempt: u64,
        started_at: i64,
        owner: &TimerRunOwner,
    ) -> Result<TimerRun, String> {
        validate_timer_identity(run_id, issue, phase, role, attempt)?;
        let owner_session_id =
            validate_owner_field(owner.session_id.as_deref(), "owner_session_id")?;
        let owner_call_id = validate_owner_field(owner.call_id.as_deref(), "owner_call_id")?;
        if let Some(existing) = self.load_timer_run(run_id)? {
            ensure_same_timer_identity(&existing, issue, phase, role, attempt)?;
            if existing.status != TIMER_STATUS_RUNNING {
                return Err(format!("timer run '{run_id}' is already finished"));
            }
            if existing.started_at != started_at {
                return Err(format!(
                    "timer run '{run_id}' was already started at a different time"
                ));
            }
            // Re-attaching an owner to an existing running row is a no-op
            // when the new values match; a mismatch on either field
            // surfaces as a structured error so two competing calls do
            // not silently overwrite each other.
            if owner_session_id.is_some()
                && existing.owner_session_id.is_some()
                && existing.owner_session_id != owner_session_id
            {
                return Err(format!(
                    "timer run '{run_id}' is already owned by another session"
                ));
            }
            if owner_call_id.is_some()
                && existing.owner_call_id.is_some()
                && existing.owner_call_id != owner_call_id
            {
                return Err(format!(
                    "timer run '{run_id}' is already owned by another call"
                ));
            }
            return Ok(existing);
        }

        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin timer start: {error}"))?;
        transaction
            .execute(
                "INSERT INTO execution_timer_runs \
                    (run_id, issue_id, phase, role, attempt, started_at, status, \
                     owner_session_id, owner_call_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    run_id,
                    issue,
                    phase,
                    role,
                    attempt as i64,
                    started_at,
                    TIMER_STATUS_RUNNING,
                    owner_session_id,
                    owner_call_id,
                ],
            )
            .map_err(|error| format!("could not persist timer start: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit timer start: {error}"))?;
        self.load_timer_run(run_id)?
            .ok_or_else(|| "timer start row disappeared after commit".to_owned())
    }

    /// Persist the finish transition and compute exact seconds plus the
    /// independently rounded Redmine hours. The operation is idempotent:
    /// retrying with the same result returns the same finished row and does
    /// not reopen or duplicate the phase.
    pub fn finish_timer_run(
        &self,
        run_id: &str,
        result: &str,
        finished_at: i64,
    ) -> Result<TimerRun, String> {
        if !["DONE", "PARTIAL", "BLOCKED", "FAILED"].contains(&result) {
            return Err(format!("invalid timer result '{result}'"));
        }
        let existing = self
            .load_timer_run(run_id)?
            .ok_or_else(|| format!("timer run '{run_id}' was not found"))?;
        // `status` is the result literal after a successful finish (DONE,
        // PARTIAL, BLOCKED, FAILED) and 'running' while the row is open.
        // A retry with the same result is the idempotent path; a retry
        // with a different result on an already-finished row is rejected
        // so a recovered orphan cannot silently overwrite the original.
        if existing.status != TIMER_STATUS_RUNNING {
            if existing.status == result {
                return Ok(existing);
            }
            return Err(format!(
                "timer run '{run_id}' is already finished as {}; \
                 cannot re-finish as {result}",
                existing.status
            ));
        }
        if finished_at < existing.started_at {
            return Err("timer finish time must not precede its start time".to_owned());
        }
        let elapsed = (finished_at - existing.started_at).max(0);
        let rounded = crate::time_tracking_cli::rounded_hours(elapsed);

        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin timer finish: {error}"))?;
        // `sync_status` is derived from the result + the durable state:
        // - `DONE`/`PARTIAL`/`BLOCKED` follow the time-entry-id rule so a
        //   successful Redmine projection that already linked an id
        //   keeps the row at `synced`; without an id the row needs a
        //   projection retry and stays at `pending`.
        // - `FAILED` always sets `sync_status='failed'` so the orphan is
        //   durably marked locally before any provider attempt. The
        //   `timer recover` path depends on this to satisfy the
        //   "durable local FAILED before provider/config lookup"
        //   invariant without an unconditional fallback in the failure
        //   path of `execute_finish`/`execute_recovery`.
        transaction
            .execute(
                "UPDATE execution_timer_runs \
                 SET finished_at = ?2, status = ?3, elapsed_seconds = ?4, rounded_hours = ?5, \
                     activity_id = COALESCE(activity_id, ?6), \
                     sync_status = CASE WHEN ?3 = 'FAILED' THEN 'failed' \
                                       WHEN redmine_time_entry_id IS NOT NULL \
                                       THEN 'synced' ELSE 'pending' END, \
                     sync_error = NULL \
                 WHERE run_id = ?1 AND status = 'running'",
                params![
                    run_id,
                    finished_at,
                    result,
                    elapsed,
                    rounded,
                    Option::<u64>::None,
                ],
            )
            .map_err(|error| format!("could not persist timer finish: {error}"))?;
        if transaction.changes() != 1 {
            return Err("timer finish lost its running row during update".to_owned());
        }
        transaction
            .commit()
            .map_err(|error| format!("could not commit timer finish: {error}"))?;
        self.load_timer_run(run_id)?
            .ok_or_else(|| "timer finish row disappeared after commit".to_owned())
    }
}
