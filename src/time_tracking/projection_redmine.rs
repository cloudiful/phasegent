use crate::infra::storage::{Storage, TimerRun};
use crate::infra::storage::{TIMER_SYNC_PROJECTING, TIMER_SYNC_SYNCED, TIMER_SYNC_UNCONFIRMED};
use crate::providers::RedmineProvider;
use crate::providers::forgejo::ForgejoError;

use super::util::format_unix_date;

/// Stable comments are both user-visible Time Entry metadata and the local
/// idempotency key used to reconcile a 204/empty response after a retry.
pub(crate) fn time_entry_comments(run: &TimerRun) -> String {
    format!("phasegent timer run_id={}", run.run_id)
}

fn timer_storage_error<'a>(operation: &'static str) -> impl FnOnce(String) -> ForgejoError + 'a {
    move |message| ForgejoError::request(operation, message)
}

pub(crate) fn project_run_with_provider(
    storage: &Storage,
    run: &mut TimerRun,
    provider: &RedmineProvider,
    token: &str,
) -> Result<(), ForgejoError> {
    if run.sync_status == TIMER_SYNC_SYNCED && run.time_entry_id.is_some() {
        return Ok(());
    }
    // Already in unconfirmed state means the POST was accepted but id is
    // missing; a retry must re-list before POST, not automatically claim
    // success. The lease still serializes concurrent re-lists.
    if run.sync_status == TIMER_SYNC_UNCONFIRMED {
        // fall through to claim handling
    }

    // Held IMMEDIATE transaction serializes the entire projection
    // (claim, activity lookup, activity persist, re-list, POST,
    // finalization). While held, a concurrent finish/recover blocks on
    // `BEGIN IMMEDIATE` and surfaces "already in progress" without ever
    // POSTing. The wall-clock lease (`PROJECTION_LEASE_SECS`) is retained
    // only for crash recovery after the lock is released; a live holder
    // is never stealable by time alone.
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

    // Ensure rollback on early exit; commit on success
    let outcome: Result<(), ForgejoError> = (|| {
        // Caller-bound lease: only the holder of `token` may POST. A loaded
        // `projecting` row without the matching token is never considered this
        // caller's claim. The token is persisted so a concurrent finish/recover
        // cannot both POST and a stale claim remains explicitly recoverable via
        // `reset_stale_projection_to_failed` after the lease window (legacy) or
        // immediately for NULL legacy rows.
        // If we already own the lease (run already projecting with our token),
        // skip the claim; otherwise attempt atomic pending/failed/unconfirmed ->
        // projecting with our token.
        if run.sync_status == TIMER_SYNC_PROJECTING
            && run.projection_token.as_deref() == Some(token)
        {
            // Already holds the lease.
        } else {
            let claimed = storage
                .try_claim_timer_projection(&run.run_id, token)
                .map_err(timer_storage_error("timer finish claim"))?;
            if !claimed {
                let current = storage
                    .load_timer_run(&run.run_id)
                    .map_err(timer_storage_error("timer finish claim"))?
                    .ok_or_else(|| ForgejoError::config("timer run disappeared during claim"))?;
                if current.sync_status == TIMER_SYNC_SYNCED && current.time_entry_id.is_some() {
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

        // Activity initialization is now covered by the held lock and the
        // owner token: two concurrent calls with `activity_id == NULL` cannot
        // both list/update and POST because only the lease holder proceeds
        // past the claim, and the activity persist is token-bound.
        if run.activity_id.is_none() {
            let activities = provider.list_time_entry_activities()?;
            let activity = RedmineProvider::select_time_entry_activity(&activities)?;
            let activity_id = activity.id;
            let ok = storage
                .update_activity_with_token(&run.run_id, token, activity_id)
                .map_err(timer_storage_error("timer finish activity selection"))?;
            if !ok {
                return Err(ForgejoError::request(
                    "timer finish",
                    "projection lease lost before activity persist".to_owned(),
                ));
            }
            *run = storage
                .load_timer_run(&run.run_id)
                .map_err(timer_storage_error("timer finish activity selection"))?
                .ok_or_else(|| {
                    ForgejoError::config("timer run disappeared after activity persist")
                })?;
        }

        let activity_id = run.activity_id.ok_or_else(|| {
            ForgejoError::config("Redmine activity id disappeared before projection")
        })?;

        let finished_at = run
            .finished_at
            .ok_or_else(|| ForgejoError::config("finished timer run has no finish timestamp"))?;
        let comments = time_entry_comments(run);
        let spent_on = format_unix_date(finished_at)?;
        let issue = run.issue;

        // Re-list before posting. Redmine can return 204/empty after accepting a
        // request, and a prior attempt may have succeeded before its response was
        // lost. The stable run marker makes that race recoverable without a
        // second Time Entry. Finalization requires the lease token so a
        // concurrent recover cannot both mark success.
        if let Some(existing) = provider.find_time_entry_by_comments(issue, &spent_on, &comments)? {
            let time_entry_id = existing.id;
            let ok = storage
                .mark_timer_sync_with_token(
                    &run.run_id,
                    token,
                    Some(activity_id),
                    Some(time_entry_id),
                    TIMER_SYNC_SYNCED,
                    None,
                )
                .map_err(timer_storage_error("timer finish reconciliation"))?;
            if !ok {
                return Err(ForgejoError::request(
                    "timer finish",
                    "projection lease lost before reconciliation".to_owned(),
                ));
            }
            *run = storage
                .load_timer_run(&run.run_id)
                .map_err(timer_storage_error("timer finish reconciliation"))?
                .ok_or_else(|| {
                    ForgejoError::config("timer run disappeared after reconciliation")
                })?;
            return Ok(());
        }

        let hours = run
            .rounded_hours
            .ok_or_else(|| ForgejoError::config("finished timer run has no rounded hours"))?;
        let created =
            provider.create_time_entry(issue, hours, &spent_on, activity_id, &comments)?;
        if let Some(entry) = created {
            let time_entry_id = entry.id;
            let ok = storage
                .mark_timer_sync_with_token(
                    &run.run_id,
                    token,
                    Some(activity_id),
                    Some(time_entry_id),
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
            // The request was accepted but Redmine supplied no id. Keep the
            // exact ledger state and allow the next finish retry to re-list
            // before considering another POST.
            let ok = storage
                .mark_timer_sync_with_token(
                    &run.run_id,
                    token,
                    Some(activity_id),
                    run.time_entry_id,
                    TIMER_SYNC_UNCONFIRMED,
                    Some("Redmine accepted the Time Entry without returning an id"),
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
