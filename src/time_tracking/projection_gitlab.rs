use crate::infra::storage::{
    Storage, TIMER_SYNC_PROJECTING, TIMER_SYNC_SYNCED, TIMER_SYNC_UNCONFIRMED, TimerRun,
};
use crate::providers::forgejo::ForgejoError;
use crate::providers::gitlab::GitlabProvider;

/// Stable marker prefix used as the GitLab `add_spent_time` summary
/// so a re-finish that already POSTed carries an obvious run-marker
/// string in the GitLab UI. The marker is for human readability; it
/// is NOT used as the idempotency key because GitLab REST v4 does
/// not surface the spent-time summary back through any listable
/// endpoint (`/notes` body contains the system event text only, not
/// the summary; `/time_stats` returns aggregate seconds).
pub(crate) const TIMER_GITLAB_MARKER_PREFIX: &str = "phasegent timer run_id=";

/// Build the GitLab spent-time summary. The leading `phasegent
/// timer run_id=` prefix makes the entry recognisable in the GitLab
/// time tracking report. The local SQLite ledger remains the source
/// of truth for idempotency because GitLab's REST API cannot
/// read back per-entry metadata.
pub(crate) fn gitlab_time_entry_summary(run: &TimerRun) -> String {
    format!("{TIMER_GITLAB_MARKER_PREFIX}{}", run.run_id)
}

fn timer_storage_error<'a>(operation: &'static str) -> impl FnOnce(String) -> ForgejoError + 'a {
    move |message| ForgejoError::request(operation, message)
}

/// Project a finished run to GitLab using `add_spent_time` with the
/// run marker as the summary.
///
/// Idempotency: the local SQLite ledger's `sync_status` column is
/// the sole marker for retry safety. GitLab REST v4 does not expose
/// per-run timelog entries (the spent-time summary is a display
/// field only and is not returned by `/notes` or `/time_stats`), so
/// any reconciliation through the API would either be unreliable
/// or indistinguishable from a different run's projection. The
/// sync_status check at the top of this function short-circuits
/// before any network call for retries on the same run id.
///
/// Crash semantics: a crash between the GitLab `add_spent_time`
/// HTTP success and the `mark_timer_sync` SQLite write causes a
/// duplicate POST on the next retry. This is a documented GitLab
/// API limitation (no idempotency-key support) and matches the
/// Redmine path's behaviour in the equivalent crash window.
///
/// `time_entry_id` is intentionally left `None` for GitLab because
/// the API does not return a numeric timelog id; Redmine keeps its
/// id-based behaviour unchanged.
pub(crate) fn project_run_with_gitlab_provider(
    storage: &Storage,
    run: &mut TimerRun,
    provider: &GitlabProvider,
    token: &str,
) -> Result<(), ForgejoError> {
    // Idempotency: the local ledger is the source of truth. A run
    // whose sync_status is already `synced` (set by a previous
    // successful projection) is treated as already-projected and
    // skipped before any HTTP traffic.
    if run.sync_status == TIMER_SYNC_SYNCED {
        return Ok(());
    }

    // Held IMMEDIATE transaction serializes GitLab projection as well:
    // a concurrent caller blocks on BEGIN and sees "already in progress"
    // without POSTing, so even though GitLab lacks a read-back marker,
    // the local ledger's `synced` guard never races.
    if let Err(error) = storage.begin_projection() {
        let lower = error.to_ascii_lowercase();
        if lower.contains("busy") || lower.contains("locked") || lower.contains("acquire") {
            return Err(ForgejoError::request(
                "timer finish",
                "projection already in progress for this run".to_owned(),
            ));
        }
        return Err(ForgejoError::request("timer finish", error));
    }

    let outcome: Result<(), ForgejoError> = (|| {
        // Caller-bound lease for GitLab as well.
        if run.sync_status == TIMER_SYNC_PROJECTING
            && run.projection_token.as_deref() == Some(token)
        {
            // already holds lease
        } else {
            let claimed = storage
                .try_claim_timer_projection(&run.run_id, token)
                .map_err(timer_storage_error("timer finish claim"))?;
            if !claimed {
                let current = storage
                    .load_timer_run(&run.run_id)
                    .map_err(timer_storage_error("timer finish claim"))?
                    .ok_or_else(|| ForgejoError::config("timer run disappeared during claim"))?;
                if current.sync_status == TIMER_SYNC_SYNCED {
                    *run = current;
                    return Ok(());
                }
                if current.sync_status == TIMER_SYNC_PROJECTING {
                    return Err(ForgejoError::request(
                        "timer finish",
                        "projection already in progress for this run".to_owned(),
                    ));
                }
                return Err(ForgejoError::request(
                    "timer finish",
                    "could not claim projection; another operation is in progress".to_owned(),
                ));
            }
            *run = storage
                .load_timer_run(&run.run_id)
                .map_err(timer_storage_error("timer finish claim"))?
                .ok_or_else(|| ForgejoError::config("timer run disappeared after claim"))?;
        }
        let elapsed = run
            .elapsed_seconds
            .ok_or_else(|| ForgejoError::config("finished timer run has no elapsed seconds"))?;
        if elapsed <= 0 {
            return Err(ForgejoError::config(
                "GitLab spent time requires a positive elapsed duration",
            ));
        }
        let summary = gitlab_time_entry_summary(run);
        // POST with the marker in the summary for UI traceability.
        // The summary is NOT used as the idempotency key: GitLab does
        // not expose per-run metadata through any listable endpoint,
        // and we never round-trip the marker for reconciliation.
        let response = provider.add_spent_time(run.issue, elapsed, Some(&summary))?;
        // `is_confirmed` accepts both the documented flat response and
        // the GitLab 19.x issue-shaped body (nested `time_stats`).
        // Without this, the live instance wraps `total_time_spent`
        // under `time_stats` and a successful POST would be marked
        // `unconfirmed`, breaking retry short-circuit.
        if response.is_confirmed() {
            let ok = storage
                .mark_timer_sync_with_token(
                    &run.run_id,
                    token,
                    run.activity_id,
                    run.time_entry_id,
                    TIMER_SYNC_SYNCED,
                    None,
                )
                .map_err(timer_storage_error("timer finish projection"))?;
            if !ok {
                return Err(ForgejoError::request(
                    "timer finish",
                    "projection lease lost before marking synced".to_owned(),
                ));
            }
            *run = storage
                .load_timer_run(&run.run_id)
                .map_err(timer_storage_error("timer finish projection"))?
                .ok_or_else(|| ForgejoError::config("timer run disappeared after projection"))?;
        } else {
            let ok = storage
                .mark_timer_sync_with_token(
                    &run.run_id,
                    token,
                    run.activity_id,
                    run.time_entry_id,
                    TIMER_SYNC_UNCONFIRMED,
                    Some("GitLab accepted the spent time without returning totals"),
                )
                .map_err(timer_storage_error("timer finish unconfirmed projection"))?;
            if !ok {
                return Err(ForgejoError::request(
                    "timer finish",
                    "projection lease lost before marking unconfirmed".to_owned(),
                ));
            }
            *run = storage
                .load_timer_run(&run.run_id)
                .map_err(timer_storage_error("timer finish unconfirmed projection"))?
                .ok_or_else(|| ForgejoError::config("timer run disappeared after unconfirmed"))?;
        }
        Ok(())
    })();

    match outcome {
        Ok(()) => {
            storage
                .commit_projection()
                .map_err(timer_storage_error("timer finish commit"))?;
            Ok(())
        }
        Err(error) => {
            let _ = storage.rollback_projection();
            Err(error)
        }
    }
}
