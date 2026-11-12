use crate::infra::storage::{Storage, TimerRunOwner};
use crate::policy::{CallerContext, Role};
use crate::providers::ProviderKind;
use crate::providers::api::PhasegentError;

use super::dispatch::TimerOutput;
use super::util::{generate_run_id, generate_run_id_with_prefix, now_epoch_seconds};

pub(crate) fn timer_orchestrator_caller(
    caller: CallerContext,
    operation: &str,
) -> Result<Option<Role>, PhasegentError> {
    // Reuse the canonical dispatch mapping (central authority and credential
    // accessors) so start/finish cannot drift; explicit agent and managed
    // errors are preserved verbatim.
    super::dispatch::timer_orchestrator_caller(caller, operation).map(Some)
}

fn timer_storage_error<'a>(operation: &'static str) -> impl FnOnce(String) -> PhasegentError + 'a {
    move |message| PhasegentError::request(operation, message)
}

#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_start(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    issue: u64,
    phase: &str,
    agent_role: String,
    attempt: u64,
    run_id: Option<&str>,
    owner: &TimerRunOwner,
) -> Result<TimerOutput, PhasegentError> {
    let caller = CallerContext::from_role(role_value);
    execute_start_with_caller(
        caller,
        provider_kind,
        issue,
        phase,
        agent_role,
        attempt,
        run_id,
        owner,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_start_with_caller(
    caller: CallerContext,
    provider_kind: Option<ProviderKind>,
    issue: u64,
    phase: &str,
    agent_role: String,
    attempt: u64,
    run_id: Option<&str>,
    owner: &TimerRunOwner,
) -> Result<TimerOutput, PhasegentError> {
    let credential = timer_orchestrator_caller(caller, "timer start")?
        .expect("authorized timer caller has a credential");
    let effective_role = normalise_agent_role(&agent_role)?;
    // The durable task of the current repository owns the timer identity: a
    // Forgejo or GitLab task has no time tracking, an ambiguous number cannot be
    // attributed to one task, and a Local task never reaches a remote
    // projection, because the ledger stores no task id. All of them refuse
    // before the ledger row is written.
    let route = crate::cli::task_route::resolve_timer_route(issue)?;
    // An unbound legacy operation still follows the selected provider, so
    // the Forgejo or GitLab time-tracking refusal applies there. A durable task pins
    // its own provider and is never blocked by a changed default.
    if matches!(route, crate::cli::task_route::TimerTaskRoute::Legacy) {
        super::dispatch::reject_unsupported_timer(credential, provider_kind)?;
    }
    let run_id = run_id.map(str::to_owned).unwrap_or_else(generate_run_id);
    let storage = Storage::open().map_err(timer_storage_error("timer start"))?;
    let started_at = now_epoch_seconds();
    let run = storage
        .start_timer_run_with_owner(
            &run_id,
            issue,
            phase,
            &effective_role,
            attempt,
            started_at,
            owner,
        )
        .map_err(timer_storage_error("timer start"))?;
    Ok(TimerOutput {
        run,
        created: true,
        sync_warning: None,
    })
}

/// Internal entry point used by the lifecycle auto-accounting path.
/// Unlike [`execute_start`] it skips the orchestrator-role check and
/// the provider kind gating because the caller (the lifecycle
/// helpers in `crate::lifecycle_auto`) has already validated those
/// invariants. The run id is server-generated with an `auto-` prefix
/// so a list/get/recover call can tell lifecycle-opened runs from
/// manual-fallback runs at a glance. The phase and role are already
/// validated by the caller, but they still flow through the
/// `start_timer_run_with_owner` validator as the final guard.
///
/// Returns the freshly-opened `run_id` so the lifecycle helper can
/// surface it in the JSON envelope without a follow-up storage read.
pub(crate) fn auto_start_run(
    issue: u64,
    phase: &str,
    role: &str,
    attempt: u64,
) -> Result<String, String> {
    debug_assert!(matches!(role, "executor" | "reviewer"));
    // Admission before any ledger write: a task whose provider has no time
    // tracking, an ambiguous current-repository task, or durable evidence
    // that spans more than one Redmine scope skips the run instead of
    // opening one the ledger could never attribute.
    crate::cli::task_route::resolve_timer_route(issue)
        .map_err(|error| format!("auto timer start refused: {error}"))?;
    let run_id = generate_run_id_with_prefix("auto");
    let started_at = now_epoch_seconds();
    let storage = Storage::open().map_err(|error| format!("auto start storage: {error}"))?;
    let _ = storage
        .start_timer_run_with_owner(
            &run_id,
            issue,
            phase,
            role,
            attempt,
            started_at,
            &TimerRunOwner::default(),
        )
        .map_err(|error| format!("auto start: {error}"))?;
    Ok(run_id)
}

fn normalise_agent_role(agent_role: &str) -> Result<String, PhasegentError> {
    let parsed = agent_role.parse::<Role>().map_err(PhasegentError::config)?;
    if !matches!(parsed, Role::Executor | Role::Reviewer) {
        return Err(PhasegentError::config(
            "timer start --agent-role must be executor or reviewer",
        ));
    }
    Ok(parsed.as_str().to_owned())
}
