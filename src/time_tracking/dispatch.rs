use crate::command::TimerCommand;
use crate::infra::storage::{Storage, TimerRun, TimerStatusFilter};
use crate::policy::Role;
use crate::providers::ProviderKind;
use crate::providers::forgejo::ForgejoError;
use serde::Serialize;

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

/// JSON returned by `timer list` and `timer get`. `list` flattens every
/// row's fields; `get` uses the same shape as a single run plus the
/// surrounding envelope so the AI can pattern-match without branching.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum TimerListOutput {
    /// `Box<TimerRun>` keeps the enum small enough to avoid the
    /// `large_enum_variant` clippy lint while keeping the on-the-wire
    /// shape identical for `list` callers.
    Single {
        run: Box<TimerRun>,
    },
    Many {
        runs: Vec<TimerRun>,
        count: usize,
    },
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
            owner_session_id,
            owner_call_id,
        } => super::start::execute_start(
            role_value,
            provider_kind,
            issue,
            &phase,
            agent_role,
            attempt,
            run_id.as_deref(),
            &crate::infra::storage::TimerRunOwner {
                session_id: owner_session_id,
                call_id: owner_call_id,
            },
        ),
        TimerCommand::Finish { run_id, result } => super::finish::execute_finish(
            role_value,
            provider_kind,
            api_base,
            project_id,
            close_status_id,
            &run_id,
            &result,
        ),
        // `list` / `get` / `recover` flow through `execute_recovery`;
        // the CLI dispatcher keeps the two paths separated so this branch
        // is unreachable in practice but kept as a defensive error.
        TimerCommand::List { .. } | TimerCommand::Get { .. } | TimerCommand::Recover { .. } => Err(
            ForgejoError::config("timer list/get/recover must be routed through execute_recovery"),
        ),
    }
}

/// Execute a `timer list` / `timer get` / `timer recover` command. These
/// three surfaces never take a `--provider`; the storage layer is the
/// source of truth and the provider only matters for `finish` /
/// `recover` projection.
pub(crate) fn execute_recovery(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: TimerCommand,
) -> Result<TimerListOutput, ForgejoError> {
    let _role = timer_orchestrator(role_value, "timer")?;
    match command {
        TimerCommand::List { status, limit } => {
            let filter = TimerStatusFilter::parse(&status).map_err(ForgejoError::config)?;
            let storage = Storage::open().map_err(timer_storage_error("timer list"))?;
            let runs = storage
                .list_timer_runs(filter, limit)
                .map_err(timer_storage_error("timer list"))?;
            let count = runs.len();
            Ok(TimerListOutput::Many { runs, count })
        }
        TimerCommand::Get { run_id } => {
            let storage = Storage::open().map_err(timer_storage_error("timer get"))?;
            let run = storage
                .load_timer_run(&run_id)
                .map_err(timer_storage_error("timer get"))?
                .ok_or_else(|| {
                    ForgejoError::config(format!("timer run '{run_id}' was not found"))
                })?;
            Ok(TimerListOutput::Single { run: Box::new(run) })
        }
        TimerCommand::Recover { run_id } => {
            let storage = Storage::open().map_err(timer_storage_error("timer recover"))?;
            super::recover::handle_recover(
                storage,
                &run_id,
                provider_kind,
                api_base,
                project_id,
                close_status_id,
            )
        }
        // `start` and `finish` are dispatched through the main entry point;
        // `execute_recovery` is its own surface for the read-only and
        // recovery commands.
        TimerCommand::Start { .. } | TimerCommand::Finish { .. } => Err(ForgejoError::config(
            "timer list/get/recover do not accept start or finish",
        )),
    }
}

pub(crate) fn timer_orchestrator(
    role_value: Option<Role>,
    operation: &str,
) -> Result<Role, ForgejoError> {
    let role = role_value
        .ok_or_else(|| ForgejoError::config(format!("{operation} requires --role orchestrator")))?;
    if role != Role::Orchestrator {
        return Err(ForgejoError::config(format!(
            "{operation} is orchestrator-only"
        )));
    }
    Ok(role)
}

pub(crate) fn timer_storage_error<'a>(
    operation: &'static str,
) -> impl FnOnce(String) -> ForgejoError + 'a {
    move |message| ForgejoError::request(operation, message)
}
