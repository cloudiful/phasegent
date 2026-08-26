use crate::command::TimerCommand;
use crate::forgejo_model::ForgejoError;
use crate::gitlab::GitlabProvider;
use crate::policy::Role;
use crate::provider::{ProviderKind, RedmineConfig, RedmineProvider};
use crate::provider_config::resolve_kind;
use crate::storage::{
    Storage, TIMER_SYNC_FAILED, TIMER_SYNC_PENDING, TIMER_SYNC_SYNCED, TIMER_SYNC_UNCONFIRMED,
    TimerRun,
};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TIMER_COUNTER: AtomicU64 = AtomicU64::new(1);

/// JSON returned by `timer start` and `timer finish`. The run fields are
/// flattened so callers do not need to know the storage implementation just
/// to pass the result to the orchestrator prompt.
#[derive(Debug, Serialize)]
pub(crate) struct TimerOutput {
    #[serde(flatten)]
    pub run: TimerRun,
    pub created: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_warning: Option<String>,
}

/// Execute a timer command. The command is intentionally orchestrator-only;
/// executor/reviewer phases are recorded by the future prompt integration and
/// are not direct callers of this CLI.
pub(crate) fn execute(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: TimerCommand,
) -> Result<TimerOutput, ForgejoError> {
    match command {
        TimerCommand::Start {
            issue,
            phase,
            agent_role,
            attempt,
            run_id,
        } => execute_start(
            role_value,
            provider_kind,
            issue,
            &phase,
            agent_role,
            attempt,
            run_id.as_deref(),
        ),
        TimerCommand::Finish { run_id, result } => execute_finish(
            role_value,
            provider_kind,
            api_base,
            project_id,
            close_status_id,
            &run_id,
            &result,
        ),
    }
}

fn execute_start(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    issue: u64,
    phase: &str,
    agent_role: String,
    attempt: u64,
    run_id: Option<&str>,
) -> Result<TimerOutput, ForgejoError> {
    let _role = timer_orchestrator(role_value, "timer start")?;
    if provider_kind == Some(ProviderKind::Forgejo) {
        return Err(ForgejoError::not_supported("forgejo", "timer start"));
    }
    let agent_role = agent_role.parse::<Role>().map_err(ForgejoError::config)?;
    if agent_role == Role::Orchestrator || agent_role == Role::Admin {
        return Err(ForgejoError::config(
            "timer start --agent-role must be executor or reviewer",
        ));
    }
    let run_id = run_id.map(str::to_owned).unwrap_or_else(generate_run_id);
    let storage = Storage::open().map_err(timer_storage_error("timer start"))?;
    let run = storage
        .start_timer_run(
            &run_id,
            issue,
            phase,
            agent_role.as_str(),
            attempt,
            now_epoch_seconds(),
        )
        .map_err(timer_storage_error("timer start"))?;
    Ok(TimerOutput {
        run,
        created: true,
        sync_warning: None,
    })
}

fn execute_finish(
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

    let projection = match project_run(
        &storage,
        &mut run,
        provider_kind,
        api_base,
        project_id,
        close_status_id,
    ) {
        Ok(()) => run,
        Err(error) => {
            let message = bounded_error_message(&error.to_string());
            let _ = storage.mark_timer_sync(
                run_id,
                run.activity_id,
                run.time_entry_id,
                TIMER_SYNC_FAILED,
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

fn project_run(
    storage: &Storage,
    run: &mut TimerRun,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<(), ForgejoError> {
    let resolved = resolve_kind(Role::Orchestrator, provider_kind)?;
    match resolved {
        ProviderKind::Redmine => {
            let provider =
                redmine_provider_for_finish(Some(resolved), api_base, project_id, close_status_id)?;
            project_run_with_provider(storage, run, &provider)
        }
        ProviderKind::Gitlab => {
            let provider = gitlab_provider_for_finish(Some(resolved), api_base, project_id)?;
            project_run_with_gitlab_provider(storage, run, &provider)
        }
        ProviderKind::Forgejo => Err(ForgejoError::not_supported("forgejo", "timer finish")),
    }
}

pub(crate) fn project_run_with_provider(
    storage: &Storage,
    run: &mut TimerRun,
    provider: &RedmineProvider,
) -> Result<(), ForgejoError> {
    if run.sync_status == TIMER_SYNC_SYNCED && run.time_entry_id.is_some() {
        return Ok(());
    }

    if run.activity_id.is_none() {
        let activities = provider.list_time_entry_activities()?;
        let activity = RedmineProvider::select_time_entry_activity(&activities)?;
        let activity_id = activity.id;
        *run = storage
            .mark_timer_sync(
                &run.run_id,
                Some(activity_id),
                run.time_entry_id,
                TIMER_SYNC_PENDING,
                None,
            )
            .map_err(timer_storage_error("timer finish activity selection"))?;
    }

    let activity_id = run
        .activity_id
        .ok_or_else(|| ForgejoError::config("Redmine activity id disappeared before projection"))?;
    let finished_at = run
        .finished_at
        .ok_or_else(|| ForgejoError::config("finished timer run has no finish timestamp"))?;
    let comments = time_entry_comments(run);
    let spent_on = format_unix_date(finished_at)?;
    let issue = run.issue;

    // Re-list before posting. Redmine can return 204/empty after accepting a
    // request, and a prior attempt may have succeeded before its response was
    // lost. The stable run marker makes that race recoverable without a
    // second Time Entry.
    if let Some(existing) = provider.find_time_entry_by_comments(issue, &spent_on, &comments)? {
        let time_entry_id = existing.id;
        *run = storage
            .mark_timer_sync(
                &run.run_id,
                Some(activity_id),
                Some(time_entry_id),
                TIMER_SYNC_SYNCED,
                None,
            )
            .map_err(timer_storage_error("timer finish reconciliation"))?;
        return Ok(());
    }

    let hours = run
        .rounded_hours
        .ok_or_else(|| ForgejoError::config("finished timer run has no rounded hours"))?;
    let created = provider.create_time_entry(issue, hours, &spent_on, activity_id, &comments)?;
    if let Some(entry) = created {
        let time_entry_id = entry.id;
        *run = storage
            .mark_timer_sync(
                &run.run_id,
                Some(activity_id),
                Some(time_entry_id),
                TIMER_SYNC_SYNCED,
                None,
            )
            .map_err(timer_storage_error("timer finish projection"))?;
    } else {
        // The request was accepted but Redmine supplied no id. Keep the
        // exact ledger state and allow the next finish retry to re-list
        // before considering another POST.
        *run = storage
            .mark_timer_sync(
                &run.run_id,
                Some(activity_id),
                run.time_entry_id,
                TIMER_SYNC_UNCONFIRMED,
                Some("Redmine accepted the Time Entry without returning an id"),
            )
            .map_err(timer_storage_error("timer finish unconfirmed projection"))?;
    }
    Ok(())
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
        crate::provider_config::GitlabConfig::resolve(Role::Orchestrator, api_base, project_id)?;
    GitlabProvider::for_role(Role::Orchestrator, config)
}

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

fn generate_run_id() -> String {
    let timestamp = now_epoch_seconds();
    let counter = NEXT_TIMER_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("timer-{timestamp:x}-{}-{:x}", std::process::id(), counter)
}

/// Round a positive exact-second duration up to a two-decimal hour. Redmine's
/// minimum granularity is 0.01 hours (36 seconds); one second therefore still
/// records as 0.01 hours, never as zero.
pub(crate) fn rounded_hours(elapsed_seconds: i64) -> f64 {
    if elapsed_seconds < 0 {
        return 0.0;
    }
    let hundredths = ((i128::from(elapsed_seconds) * 100 + 3599) / 3600).max(1);
    hundredths as f64 / 100.0
}

/// Stable comments are both user-visible Time Entry metadata and the local
/// idempotency key used to reconcile a 204/empty response after a retry.
pub(crate) fn time_entry_comments(run: &TimerRun) -> String {
    format!("phasegent timer run_id={}", run.run_id)
}

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
) -> Result<(), ForgejoError> {
    // Idempotency: the local ledger is the source of truth. A run
    // whose sync_status is already `synced` (set by a previous
    // successful projection) is treated as already-projected and
    // skipped before any HTTP traffic.
    if run.sync_status == TIMER_SYNC_SYNCED {
        return Ok(());
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
        *run = storage
            .mark_timer_sync(
                &run.run_id,
                run.activity_id,
                run.time_entry_id,
                TIMER_SYNC_SYNCED,
                None,
            )
            .map_err(timer_storage_error("timer finish projection"))?;
    } else {
        *run = storage
            .mark_timer_sync(
                &run.run_id,
                run.activity_id,
                run.time_entry_id,
                TIMER_SYNC_UNCONFIRMED,
                Some("GitLab accepted the spent time without returning totals"),
            )
            .map_err(timer_storage_error("timer finish unconfirmed projection"))?;
    }
    Ok(())
}

fn now_epoch_seconds() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

pub(crate) fn format_unix_date(timestamp: i64) -> Result<String, ForgejoError> {
    let days = timestamp.div_euclid(86_400);
    // Howard Hinnant's civil_from_days algorithm, without adding a date
    // crate solely for this small projection.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

fn bounded_error_message(message: &str) -> String {
    message.chars().take(512).collect()
}
