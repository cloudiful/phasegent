use crate::infra::storage::{Storage, TimerRunOwner};
use crate::policy::Role;
use crate::providers::ProviderKind;
use crate::providers::api::PhasegentError;

use super::dispatch::TimerOutput;
use super::util::{generate_run_id, generate_run_id_with_prefix, now_epoch_seconds};

fn timer_orchestrator(role_value: Option<Role>, operation: &str) -> Result<Role, PhasegentError> {
    let role = role_value.ok_or_else(|| {
        PhasegentError::config(format!(
            "{operation} requires the orchestrator role; set PHASEGENT_ROLE=orchestrator"
        ))
    })?;
    if role != Role::Orchestrator {
        return Err(PhasegentError::config(format!(
            "{operation} is orchestrator-only"
        )));
    }
    Ok(role)
}

fn timer_storage_error<'a>(operation: &'static str) -> impl FnOnce(String) -> PhasegentError + 'a {
    move |message| PhasegentError::request(operation, message)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_start(
    role_value: Option<Role>,
    _provider_kind: Option<ProviderKind>,
    issue: u64,
    phase: &str,
    agent_role: String,
    attempt: u64,
    run_id: Option<&str>,
    owner: &TimerRunOwner,
) -> Result<TimerOutput, PhasegentError> {
    let _role = timer_orchestrator(role_value, "timer start")?;
    let effective_role = normalise_agent_role(&agent_role)?;
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
