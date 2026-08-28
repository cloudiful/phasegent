#[allow(unused_imports)]
use super::parse_helpers::{
    has_flag, optional_option, planning_options, positional_number, require_exact_positionals,
    required_nonempty_option, required_option, required_value, split_inline, validate_options,
};
#[allow(unused_imports)]
use super::{
    Command, CommentCommand, HelpTopic, Invocation, IssueCommand, PlanningOptions, ProjectCommand,
    RelationCommand, StatusCommand, TimerCommand, VersionCommand, WorkflowCommand,
};
#[allow(unused_imports)]
use crate::policy::Role;
#[allow(unused_imports)]
use crate::providers::ProviderKind;
#[allow(unused_imports)]
use crate::providers::api::ForgejoError;
#[allow(unused_imports)]
use crate::providers::redmine::model::RedmineRelationType;

pub(crate) fn parse_workflow(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Workflow));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::WorkflowCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "bootstrap" => {
            // The shared `AI Agents` group has been retired. Surface a clear
            // usage error so stale callers do not silently get the new
            // behaviour.
            for rejected in ["--group-name", "--group-role"] {
                if args.iter().any(|value| value == rejected)
                    || args
                        .iter()
                        .any(|value| split_inline(value, rejected).is_some())
                {
                    return Err(format!(
                        "workflow bootstrap {rejected} is no longer supported; \
                         direct memberships are reconciled automatically from the \
                         orchestrator, executor, and reviewer API keys",
                    ));
                }
            }
            validate_options(
                args,
                0,
                &["--repository", "--close-status-id", "--close-status-name"],
                &[],
                "workflow bootstrap",
            )?;
            let close_status_id = optional_option(args, "--close-status-id");
            let close_status_name = optional_option(args, "--close-status-name");
            if close_status_id.is_some() && close_status_name.is_some() {
                return Err(
                    "workflow bootstrap accepts either --close-status-id or --close-status-name"
                        .to_owned(),
                );
            }
            Ok(Command::Workflow(WorkflowCommand::Bootstrap {
                repository: optional_option(args, "--repository"),
                close_status_id,
                close_status_name,
            }))
        }
        value => Err(format!("unknown workflow command '{value}'")),
    }
}
