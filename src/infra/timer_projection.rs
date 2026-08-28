use crate::infra::storage::Storage;
use crate::infra::timer_ledger::{
    PROJECTION_LEASE_SECS, TIMER_SYNC_FAILED, TIMER_SYNC_PENDING, TIMER_SYNC_PROJECTING,
    TIMER_SYNC_UNCONFIRMED, TimerRun, now_epoch_seconds, valid_timer_sync_status,
    validate_projection_token,
};
use rusqlite::params;

impl Storage {
    pub fn try_claim_timer_projection(&self, run_id: &str, token: &str) -> Result<bool, String> {
        validate_projection_token(token)?;
        let now = now_epoch_seconds();
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET sync_status = ?1, projection_token = ?2, projection_claimed_at = ?3 \
                 WHERE run_id = ?4 \
                   AND (sync_status = ?5 OR sync_status = ?6 OR sync_status = ?7)",
                params![
                    TIMER_SYNC_PROJECTING,
                    token,
                    now,
                    run_id,
                    TIMER_SYNC_PENDING,
                    TIMER_SYNC_FAILED,
                    TIMER_SYNC_UNCONFIRMED
                ],
            )
            .map_err(|error| format!("could not claim timer projection: {error}"))?;
        Ok(changed == 1)
    }

    /// Reset a `projecting` row back to `failed` when the caller presents
    /// the matching lease token. The token check guarantees a concurrent
    /// live projector cannot be reset by a second caller. Use
    /// `reset_stale_projection_to_failed` for the hard-crash recovery path
    /// where the original token is no longer available.
    #[allow(dead_code)]
    pub fn reset_projecting_to_failed(
        &self,
        run_id: &str,
        token: &str,
        error: &str,
    ) -> Result<bool, String> {
        validate_projection_token(token)?;
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET sync_status = ?1, sync_error = ?2, projection_token = NULL, projection_claimed_at = NULL \
                 WHERE run_id = ?3 AND sync_status = ?4 AND projection_token = ?5",
                params![TIMER_SYNC_FAILED, error, run_id, TIMER_SYNC_PROJECTING, token],
            )
            .map_err(|error| format!("could not reset projecting timer: {error}"))?;
        Ok(changed == 1)
    }

    /// Force-reset a stale `projecting` claim that is older than the lease
    /// window or has a NULL claimed_at (legacy). This is the explicit
    /// `timer recover` recovery path for a hard-crash orphan: the caller
    /// does not hold the token but can recover after the lease expires.
    /// A live projector whose claim is still within the lease is not reset.
    ///
    /// # Liveness invariant and legacy compatibility
    ///
    /// The reset acquires an `IMMEDIATE` SQLite transaction first so it
    /// cannot race against a live projector that is also holding one.
    /// While the live projector holds its `IMMEDIATE` (from claim through
    /// token-bound finalization), the reset blocks on `busy`/`locked`,
    /// observes the row state after the live projector commits or rolls
    /// back, and never observes a mid-flight `projecting` row owned by
    /// another caller. The wall-clock check is therefore enforced in
    /// addition to the lock so the reset only fires on a truly abandoned
    /// row (live crash that left an autocommit or legacy `NULL` claim).
    /// A modern hard crash that was inside a held transaction rolls the
    /// `projecting` claim back to the pre-claim state on process exit, so
    /// no stale row remains for this reset to discover; only legacy or
    /// autocommit crash windows leave a `projecting` row recoverable.
    ///
    /// # Return shape
    ///
    /// Returns `Ok(true)` when the row was reset, `Ok(false)` when the
    /// row is held by a live lease inside a held `IMMEDIATE` (and the
    /// caller should surface "projection already in progress"), and
    /// `Ok(false)` when the row is no longer in the `projecting` state.
    /// Returns `Err` only on storage failures.
    pub fn reset_stale_projection_to_failed(
        &self,
        run_id: &str,
        error: &str,
    ) -> Result<bool, String> {
        // Acquire `IMMEDIATE` first so the reset blocks against a live
        // holder that is also inside an `IMMEDIATE`. The held lock is
        // the liveness signal; time alone never makes a live holder
        // stealable. We retry `busy`/`locked` with bounded backoff so a
        // legitimate wait for the live holder's commit or rollback is
        // not falsely reported as a reset failure.
        self.begin_projection()?;
        let now = now_epoch_seconds();
        let threshold = now - PROJECTION_LEASE_SECS;
        let outcome: Result<bool, String> = (|| {
            // Re-read inside the held IMMEDIATE so we observe the
            // post-commit/post-rollback state of any concurrent live
            // holder. The lease window check is the documented hard-
            // crash legacy path; a live holder that survived its
            // transaction is by definition outside this window only
            // after it has either succeeded (sync_status != projecting)
            // or rolled back (sync_status != projecting).
            let changed = self
                .connection
                .execute(
                    "UPDATE execution_timer_runs \
                     SET sync_status = ?1, sync_error = ?2, projection_token = NULL, projection_claimed_at = NULL \
                     WHERE run_id = ?3 AND sync_status = ?4 \
                       AND (projection_claimed_at IS NULL OR projection_claimed_at <= ?5)",
                    params![
                        TIMER_SYNC_FAILED,
                        error,
                        run_id,
                        TIMER_SYNC_PROJECTING,
                        threshold
                    ],
                )
                .map_err(|error| format!("could not reset stale projecting timer: {error}"))?;
            Ok(changed == 1)
        })();
        match outcome {
            Ok(value) => {
                self.commit_projection()?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.rollback_projection();
                Err(error)
            }
        }
    }

    /// Acquire an `IMMEDIATE` transaction that serializes the entire
    /// projection (claim, activity lookup, re-list, POST, finalization).
    /// While held, a concurrent `BEGIN IMMEDIATE` on another connection
    /// receives `busy`/`locked` and must surface "already in progress"
    /// without POST. The lock is released by `commit_projection` or
    /// `rollback_projection`. Retries `busy` a few times before surfacing.
    pub(crate) fn begin_projection(&self) -> Result<(), String> {
        let mut attempts = 0;
        loop {
            match self.connection.execute("BEGIN IMMEDIATE", []) {
                Ok(_) => return Ok(()),
                Err(error) => {
                    let msg = error.to_string().to_ascii_lowercase();
                    let is_busy = msg.contains("busy") || msg.contains("locked");
                    attempts += 1;
                    if is_busy && attempts < 5 {
                        std::thread::sleep(std::time::Duration::from_millis(10 * attempts as u64));
                        continue;
                    }
                    return Err(format!("could not acquire projection lock: {error}"));
                }
            }
        }
    }

    pub(crate) fn commit_projection(&self) -> Result<(), String> {
        self.connection
            .execute("COMMIT", [])
            .map_err(|error| format!("could not commit projection: {error}"))?;
        Ok(())
    }

    pub(crate) fn rollback_projection(&self) -> Result<(), String> {
        let _ = self.connection.execute("ROLLBACK", []);
        Ok(())
    }

    /// Persist `activity_id` while holding the projection lease. The row
    /// must still be `projecting` and the `token` must match, so a
    /// concurrent holder cannot overwrite the activity.
    pub(crate) fn update_activity_with_token(
        &self,
        run_id: &str,
        token: &str,
        activity_id: u64,
    ) -> Result<bool, String> {
        validate_projection_token(token)?;
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET activity_id = ?2 \
                 WHERE run_id = ?1 AND projection_token = ?3 AND sync_status = ?4",
                params![run_id, activity_id as i64, token, TIMER_SYNC_PROJECTING],
            )
            .map_err(|error| format!("could not update activity with token: {error}"))?;
        Ok(changed == 1)
    }

    /// Finalize a claimed projection when the caller holds the lease token.
    /// The update succeeds only when `projection_token` matches and the row
    /// is still `projecting`. On transition out of `projecting` the token
    /// and claimed_at are cleared so a retry can claim again. Callers that
    /// do not hold the token receive `Ok(false)` semantics via the
    /// `changes == 0` path and should surface "projection already in
    /// progress" rather than overwriting another claim.
    pub fn mark_timer_sync_with_token(
        &self,
        run_id: &str,
        token: &str,
        activity_id: Option<u64>,
        time_entry_id: Option<u64>,
        sync_status: &str,
        sync_error: Option<&str>,
    ) -> Result<bool, String> {
        if !valid_timer_sync_status(sync_status) {
            return Err(format!("invalid timer sync status '{sync_status}'"));
        }
        if sync_status == TIMER_SYNC_FAILED && sync_error.is_none_or(str::is_empty) {
            return Err("timer sync failure requires a non-empty error".to_owned());
        }
        validate_projection_token(token)?;
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET activity_id = COALESCE(activity_id, ?2), \
                     redmine_time_entry_id = COALESCE(redmine_time_entry_id, ?3), \
                     sync_status = ?4, sync_error = ?5, \
                     projection_token = CASE WHEN ?4 IN ('synced','failed','unconfirmed') THEN NULL ELSE projection_token END, \
                     projection_claimed_at = CASE WHEN ?4 IN ('synced','failed','unconfirmed') THEN NULL ELSE projection_claimed_at END \
                 WHERE run_id = ?1 AND projection_token = ?6 AND sync_status = ?7",
                params![
                    run_id,
                    activity_id,
                    time_entry_id,
                    sync_status,
                    sync_error,
                    token,
                    TIMER_SYNC_PROJECTING
                ],
            )
            .map_err(|error| format!("could not persist timer sync with token: {error}"))?;
        Ok(changed == 1)
    }

    /// Record `sync_error` on a row whose local finish is already
    /// `FAILED`. This is the documented durable local FAILED step used
    /// by `timer recover` after a projection attempt fails: the row's
    /// `status` is `FAILED` (set by `finish_timer_run`) and its
    /// `sync_status` is `failed` (also set there) until a future
    /// successful claim. The update only fires when the row is in a
    /// state the caller exclusively owns (`failed` is not `projecting`)
    /// so a concurrent live holder is never overwritten. The error
    /// message is bounded to keep audit logs compact.
    pub fn record_failed_sync_error(&self, run_id: &str, error: &str) -> Result<bool, String> {
        let trimmed = error.chars().take(512).collect::<String>();
        let changed = self
            .connection
            .execute(
                "UPDATE execution_timer_runs \
                 SET sync_error = ?2 \
                 WHERE run_id = ?1 AND status IN ('FAILED', 'DONE', 'PARTIAL', 'BLOCKED') \
                   AND sync_status != ?3",
                params![run_id, trimmed, TIMER_SYNC_PROJECTING],
            )
            .map_err(|error| format!("could not record failed sync error: {error}"))?;
        Ok(changed == 1)
    }

    /// Advance the Redmine synchronization state after a local finish.  The
    /// coalescing assignments make retries safe even when a caller has only
    /// an activity id or only a time-entry id. Production callers should
    /// prefer `mark_timer_sync_with_token` so the transition is bound to
    /// the lease holder; this entry point remains `pub` for tests and
    /// external integrations that explicitly opt out of the lease
    /// protocol (and therefore accept the corresponding concurrency
    /// risk).
    #[allow(dead_code)]
    pub fn mark_timer_sync(
        &self,
        run_id: &str,
        activity_id: Option<u64>,
        time_entry_id: Option<u64>,
        sync_status: &str,
        sync_error: Option<&str>,
    ) -> Result<TimerRun, String> {
        if !valid_timer_sync_status(sync_status) {
            return Err(format!("invalid timer sync status '{sync_status}'"));
        }
        if sync_status == TIMER_SYNC_FAILED && sync_error.is_none_or(str::is_empty) {
            return Err("timer sync failure requires a non-empty error".to_owned());
        }
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin timer sync: {error}"))?;
        transaction
            .execute(
                "UPDATE execution_timer_runs \
                 SET activity_id = COALESCE(activity_id, ?2), \
                     redmine_time_entry_id = COALESCE(redmine_time_entry_id, ?3), \
                     sync_status = ?4, sync_error = ?5 \
                 WHERE run_id = ?1",
                params![run_id, activity_id, time_entry_id, sync_status, sync_error],
            )
            .map_err(|error| format!("could not persist timer sync: {error}"))?;
        if transaction.changes() != 1 {
            return Err(format!("timer run '{run_id}' was not found"));
        }
        transaction
            .commit()
            .map_err(|error| format!("could not commit timer sync: {error}"))?;
        self.load_timer_run(run_id)?
            .ok_or_else(|| "timer sync row disappeared after commit".to_owned())
    }
}
