use crate::infra::storage::{Storage, TIMER_STATUS_RUNNING};
use crate::policy::{CallerContext, Role};
use crate::providers::api::PhasegentError;

use super::dispatch::TimerListOutput;
use super::finish::project_run_with_credential;
use super::util::{bounded_error_message, generate_projection_token, now_epoch_seconds};

fn timer_storage_error<'a>(operation: &'static str) -> impl FnOnce(String) -> PhasegentError + 'a {
    move |message| PhasegentError::request(operation, message)
}

/// Read-only admission for one recovery.
///
/// The run row is read without creating the database, running a migration or
/// mutating a row, only to learn the issue number whose current-repository
/// timer route must be decided before the ledger is opened for writing.
/// `Ok(None)` means there is no readable run yet, so the legacy path keeps
/// its existing not-found behavior.
pub(crate) fn admit_recovery(
    run_id: &str,
) -> Result<Option<crate::cli::task_route::TimerTaskRoute>, PhasegentError> {
    let storage = Storage::open_existing_read_only().map_err(PhasegentError::config)?;
    let Some(storage) = storage else {
        return Ok(None);
    };
    if !storage
        .table_exists("execution_timer_runs")
        .map_err(PhasegentError::config)?
    {
        return Ok(None);
    }
    let Some(run) = storage
        .load_timer_run(run_id)
        .map_err(timer_storage_error("timer recover"))?
    else {
        return Ok(None);
    };
    Ok(Some(crate::cli::task_route::resolve_timer_route(
        run.issue,
    )?))
}

#[allow(dead_code)]
pub(crate) fn handle_recover(
    storage: Storage,
    run_id: &str,
    provider_kind: Option<crate::providers::ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<TimerListOutput, PhasegentError> {
    handle_recover_with_caller(
        CallerContext::Agent(Role::Orchestrator),
        storage,
        run_id,
        provider_kind,
        api_base,
        project_id,
        close_status_id,
    )
}

pub(crate) fn handle_recover_with_caller(
    caller: CallerContext,
    storage: Storage,
    run_id: &str,
    provider_kind: Option<crate::providers::ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<TimerListOutput, PhasegentError> {
    let credential = match caller.credential_role() {
        Some(role) => role,
        None => {
            return Err(PhasegentError::config(crate::cli::managed_denial_message(
                "timer recover",
            )));
        }
    };
    let existing = storage
        .load_timer_run(run_id)
        .map_err(timer_storage_error("timer recover"))?
        .ok_or_else(|| PhasegentError::config(format!("timer run '{run_id}' was not found")))?;
    // Admission before any mutable step: the current-repository task route
    // owns the timer identity, so a bound Forgejo task, an ambiguous task or
    // conflicting durable evidence refuses before the stale-claim reset,
    // the FAILED transition or any projection. A bound Redmine task pins the
    // projection to its own instance and project; a bound Local task keeps
    // the run local; the legacy path keeps the caller's selection.
    let route = crate::cli::task_route::resolve_timer_route(existing.issue)?;
    let (provider_kind, api_base, project_id) =
        super::finish::projection_target(Some(&route), provider_kind, api_base, project_id);
    let api_base = api_base.as_deref();
    let project_id = project_id.as_deref();
    if existing.status != TIMER_STATUS_RUNNING {
        // Terminal rows: a `projecting` row is a lease. Only a
        // stale lease (expired or legacy NULL claimed_at) may be
        // force-reset; a live lease must not be cleared by a
        // concurrent recover, otherwise the live projector could
        // later overwrite or be aborted. The `reset_stale` check
        // enforces the lease window (PROJECTION_LEASE_SECS) for
        // legacy compatibility; modern rows hold an IMMEDIATE
        // transaction, so time alone never makes a live holder
        // stealable.
        if existing.sync_status == crate::infra::storage::TIMER_SYNC_PROJECTING {
            drop(storage);
            let storage = Storage::open().map_err(timer_storage_error("timer recover"))?;
            let reset = storage
                .reset_stale_projection_to_failed(
                    run_id,
                    "recovery found stale projecting claim; resetting for retry",
                )
                .map_err(timer_storage_error("timer recover"))?;
            if reset {
                // Stale recovered: continue through token-bound
                // projection retry path below instead of returning
                // immediately with failed state.
                let storage = Storage::open().map_err(timer_storage_error("timer recover"))?;
                let mut run_mut = storage
                    .load_timer_run(run_id)
                    .map_err(timer_storage_error("timer recover"))?
                    .ok_or_else(|| {
                        PhasegentError::config(format!("timer run '{run_id}' was not found"))
                    })?;
                let token = generate_projection_token();
                match project_run_with_credential(
                    credential,
                    &storage,
                    &mut run_mut,
                    provider_kind,
                    api_base,
                    project_id,
                    close_status_id,
                    &token,
                ) {
                    Ok(()) => {
                        let storage =
                            Storage::open().map_err(timer_storage_error("timer recover"))?;
                        let final_run = storage
                            .load_timer_run(run_id)
                            .map_err(timer_storage_error("timer recover"))?
                            .ok_or_else(|| {
                                PhasegentError::config(format!(
                                    "timer run '{run_id}' was not found"
                                ))
                            })?;
                        return Ok(TimerListOutput::Single {
                            run: Box::new(final_run),
                        });
                    }
                    Err(error) => {
                        let message = bounded_error_message(&error.to_string());
                        // Owner-bound failure: only the lease holder
                        // may transition projecting->failed. If we
                        // never acquired the lease we MUST NOT mutate
                        // `sync_status`; the row already carries the
                        // durable `failed` state from
                        // `reset_stale_projection_to_failed` above.
                        // We do record the projection error on the
                        // terminal row via `record_failed_sync_error`,
                        // which is safe because it requires
                        // `sync_status != projecting` and therefore
                        // never overwrites a live lease.
                        let _ = storage.record_failed_sync_error(run_id, &message);
                        return Err(error);
                    }
                }
            }
            return Err(PhasegentError::request(
                "timer recover",
                "projection already in progress for this run".to_owned(),
            ));
        }
        if existing.sync_status == crate::infra::storage::TIMER_SYNC_FAILED {
            let message = existing
                .sync_error
                .clone()
                .unwrap_or_else(|| "timer recover: previous projection failed".to_owned());
            return Err(PhasegentError::request("timer recover", message));
        }
        return Ok(TimerListOutput::Single {
            run: Box::new(existing),
        });
    }
    drop(storage);
    // Durable local FAILED transition before any provider check.
    // This guarantees the orphan is never left running, even when
    // Redmine config is missing.
    let storage = Storage::open().map_err(timer_storage_error("timer recover"))?;
    let finished_at = now_epoch_seconds();
    let run = match storage.finish_timer_run(run_id, "FAILED", finished_at) {
        Ok(row) => row,
        Err(message) if message.contains("already finished") => {
            let storage = Storage::open().map_err(timer_storage_error("timer recover"))?;
            let row = storage
                .load_timer_run(run_id)
                .map_err(timer_storage_error("timer recover"))?
                .ok_or_else(|| {
                    PhasegentError::config(format!("timer run '{run_id}' was not found"))
                })?;
            if row.sync_status == crate::infra::storage::TIMER_SYNC_FAILED {
                let msg = row
                    .sync_error
                    .clone()
                    .unwrap_or_else(|| "timer recover: previous projection failed".to_owned());
                return Err(PhasegentError::request("timer recover", msg));
            }
            if row.sync_status == crate::infra::storage::TIMER_SYNC_PROJECTING {
                return Err(PhasegentError::request(
                    "timer recover",
                    "concurrent recovery already claimed this run".to_owned(),
                ));
            }
            return Ok(TimerListOutput::Single { run: Box::new(row) });
        }
        Err(message) => {
            return Err(PhasegentError::request("timer recover", message));
        }
    };
    // Provider projection with caller-bound lease token. The token is
    // generated per invocation so a second concurrent recover cannot
    // reuse the loaded `projecting` row; it must successfully claim
    // its own lease. `project_run` handles the atomic claim and
    // requires the same token for finalization.
    let token = generate_projection_token();
    let storage = Storage::open().map_err(timer_storage_error("timer recover"))?;
    // Refresh after finish so the in-memory row reflects the durable
    // FAILED finish and any concurrent claim. Pass the fresh token
    // into the projection path.
    let mut run_mut = storage
        .load_timer_run(run_id)
        .map_err(timer_storage_error("timer recover"))?
        .ok_or_else(|| PhasegentError::config(format!("timer run '{run_id}' was not found")))?;
    // If the row is already terminal synced, `project_run` will
    // short-circuit; otherwise it will attempt the lease claim with
    // `token`. A hard-crash stale lease is handled via the
    // explicit stale-reset path above on the next retry.
    let _ = run; // keep original for potential token fallback
    match project_run_with_credential(
        credential,
        &storage,
        &mut run_mut,
        provider_kind,
        api_base,
        project_id,
        close_status_id,
        &token,
    ) {
        Ok(()) => {
            let storage = Storage::open().map_err(timer_storage_error("timer recover"))?;
            let final_run = storage
                .load_timer_run(run_id)
                .map_err(timer_storage_error("timer recover"))?
                .ok_or_else(|| {
                    PhasegentError::config(format!("timer run '{run_id}' was not found"))
                })?;
            if final_run.status == TIMER_STATUS_RUNNING {
                return Err(PhasegentError::request(
                    "timer recover",
                    "recovery left row running".to_owned(),
                ));
            }
            Ok(TimerListOutput::Single {
                run: Box::new(final_run),
            })
        }
        Err(error) => {
            let message = bounded_error_message(&error.to_string());
            // Owner-bound failure: only the lease holder may
            // transition projecting->failed. If we never acquired
            // the lease we MUST NOT mutate `sync_status`; a
            // concurrent live holder may still be holding
            // `projecting`. The row already carries `failed` from
            // `finish_timer_run` locally before any provider
            // attempt, so the durable state is correct. We do
            // record the projection error on the terminal row
            // via `record_failed_sync_error`, which is safe
            // because it requires `sync_status != projecting`
            // and therefore never overwrites a live lease.
            let _ = storage.record_failed_sync_error(run_id, &message);
            Err(error)
        }
    }
}
