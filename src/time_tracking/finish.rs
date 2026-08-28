use crate::infra::storage::{Storage, TIMER_SYNC_SYNCED, TIMER_SYNC_UNCONFIRMED, TimerRun};
use crate::policy::Role;
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::gitlab::GitlabProvider;
use crate::providers::{ProviderKind, RedmineConfig, RedmineProvider};

use super::dispatch::TimerOutput;
use super::util::{bounded_error_message, generate_projection_token, now_epoch_seconds};

fn timer_orchestrator(role_value: Option<Role>, operation: &str) -> Result<Role, ForgejoError> {
    let role = role_value
        .ok_or_else(|| ForgejoError::config(format!("{operation} requires --role orchestrator")))?;
    if role != Role::Orchestrator {
        return Err(ForgejoError::config(format!(
            "{operation} is orchestrator-only"
        )));
    }
    Ok(role)
}

fn timer_storage_error<'a>(operation: &'static str) -> impl FnOnce(String) -> ForgejoError + 'a {
    move |message| ForgejoError::request(operation, message)
}

pub(crate) fn execute_finish(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    run_id: &str,
    result: &str,
) -> Result<TimerOutput, ForgejoError> {
    let _role = timer_orchestrator(role_value, "timer finish")?;
    if provider_kind == Some(ProviderKind::Forgejo) {
        return Err(ForgejoError::not_supported("forgejo", "timer finish"));
    }

    // This local transition deliberately precedes every provider/key lookup
    // and every network request. A failed projection is recoverable by
    // retrying the same run id.
    let storage = Storage::open().map_err(timer_storage_error("timer finish"))?;
    let finished_at = now_epoch_seconds();
    let mut run = storage
        .finish_timer_run(run_id, result, finished_at)
        .map_err(timer_storage_error("timer finish"))?;

    // The early-return check is provider-aware: Redmine uses the
    // numeric time-entry id as the durable idempotency key, so the
    // row must carry a non-null id before the projection is skipped.
    // GitLab has no equivalent remote id and reconciles retries via a
    // run-marker embedded in the spent-time summary; the projection
    // path therefore only needs `sync_status == synced` to skip.
    let already_projected = match provider_kind {
        Some(ProviderKind::Gitlab) => run.sync_status == TIMER_SYNC_SYNCED,
        _ => run.sync_status == TIMER_SYNC_SYNCED && run.time_entry_id.is_some(),
    };
    if already_projected {
        return Ok(TimerOutput {
            run,
            created: false,
            sync_warning: None,
        });
    }

    // Caller-bound lease token: only the holder may transition
    // pending/failed/unconfirmed -> projecting and later finalize the
    // projection. A second concurrent finish that loads a terminal
    // `projecting` row without the token must not be treated as owning
    // the claim.
    let token = generate_projection_token();
    let projection = match project_run(
        &storage,
        &mut run,
        provider_kind,
        api_base,
        project_id,
        close_status_id,
        &token,
    ) {
        Ok(()) => run,
        Err(error) => {
            let message = bounded_error_message(&error.to_string());
            // Owner-bound failure: only the lease holder may mark
            // projecting->failed. A caller that did not acquire ownership
            // must NOT destroy the live owner's ability to finalize.
            // The unconditional fallback was the P1 race in round 3:
            // between the `load_timer_run` liveness check and the
            // `mark_timer_sync` write a concurrent caller could claim
            // the row, and the unconditional mark would clobber the new
            // holder's `projecting` state. If we never acquired the lease
            // we leave the row alone; it already carries the durable
            // `pending`/`failed`/`unconfirmed` state from `finish_timer_run`
            // locally before any provider attempt, so the structured
            // error is the only observable signal of the projection
            // failure. The next retry with the same run id reuses the
            // marker-based reconciliation before any POST. If the user
            // explicitly passed `--result FAILED` the row is already at
            // `sync_status='failed'`; otherwise it stays at `pending` so
            // a future retry can still attempt projection.
            let _ = storage.mark_timer_sync_with_token(
                run_id,
                &token,
                run.activity_id,
                run.time_entry_id,
                crate::infra::storage::TIMER_SYNC_FAILED,
                Some(&message),
            );
            return Err(error);
        }
    };
    let sync_warning = (projection.sync_status == TIMER_SYNC_UNCONFIRMED).then(|| {
        match provider_kind {
            Some(ProviderKind::Gitlab) => {
                "GitLab accepted the spent time without returning totals; retry reconciliation before creating another entry"
                    .to_owned()
            }
            _ => "Redmine accepted the Time Entry without returning an id; retry reconciliation before creating another entry".to_owned(),
        }
    });
    Ok(TimerOutput {
        run: projection,
        created: false,
        sync_warning,
    })
}

pub(crate) fn project_run(
    storage: &Storage,
    run: &mut TimerRun,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    token: &str,
) -> Result<(), ForgejoError> {
    let resolved = resolve_kind(Role::Orchestrator, provider_kind)?;
    match resolved {
        ProviderKind::Redmine => {
            let provider =
                redmine_provider_for_finish(Some(resolved), api_base, project_id, close_status_id)?;
            super::projection_redmine::project_run_with_provider(storage, run, &provider, token)
        }
        ProviderKind::Gitlab => {
            let provider = gitlab_provider_for_finish(Some(resolved), api_base, project_id)?;
            super::projection_gitlab::project_run_with_gitlab_provider(
                storage, run, &provider, token,
            )
        }
        ProviderKind::Forgejo => Err(ForgejoError::not_supported("forgejo", "timer finish")),
    }
}

fn require_redmine_provider(
    provider_kind: Option<ProviderKind>,
    operation: &str,
) -> Result<(), ForgejoError> {
    let resolved_provider = resolve_kind(Role::Orchestrator, provider_kind)?;
    if resolved_provider != ProviderKind::Redmine {
        return Err(ForgejoError::not_supported("forgejo", operation));
    }
    Ok(())
}

fn require_gitlab_provider(
    provider_kind: Option<ProviderKind>,
    operation: &str,
) -> Result<(), ForgejoError> {
    let resolved_provider = resolve_kind(Role::Orchestrator, provider_kind)?;
    if resolved_provider != ProviderKind::Gitlab {
        return Err(ForgejoError::not_supported(
            resolved_provider.as_str(),
            operation,
        ));
    }
    Ok(())
}

fn redmine_provider_for_finish(
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<RedmineProvider, ForgejoError> {
    // Provider and API-base arguments are resolved here, after the local
    // finish transition, so no remote call can occur before durable state.
    // The caller has already selected the orchestrator role.
    require_redmine_provider(provider_kind, "timer finish")?;
    let config = RedmineConfig::resolve(Role::Orchestrator, api_base, project_id, close_status_id)?;
    RedmineProvider::for_role(Role::Orchestrator, config)
}

fn gitlab_provider_for_finish(
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
) -> Result<GitlabProvider, ForgejoError> {
    // The provider and api-base arguments are resolved here, after the
    // local finish transition, so no remote call can occur before
    // durable state. The caller has already selected the orchestrator
    // role.
    require_gitlab_provider(provider_kind, "timer finish")?;
    let config =
        crate::providers::config::GitlabConfig::resolve(Role::Orchestrator, api_base, project_id)?;
    GitlabProvider::for_role(Role::Orchestrator, config)
}
