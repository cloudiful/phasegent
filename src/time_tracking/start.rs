use crate::infra::storage::{Storage, TimerRunOwner};
use crate::policy::Role;
use crate::providers::ProviderKind;
use crate::providers::forgejo::ForgejoError;

use super::dispatch::TimerOutput;
use super::util::{generate_run_id, now_epoch_seconds};

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
    let started_at = now_epoch_seconds();
    let run = storage
        .start_timer_run_with_owner(
            &run_id,
            issue,
            phase,
            agent_role.as_str(),
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
