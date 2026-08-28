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

pub(crate) fn parse_status(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Status));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::StatusCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 1, "status list")?;
            Ok(Command::Status(StatusCommand::List))
        }
        "next" => {
            require_exact_positionals(args, 2, "status next")?;
            Ok(Command::Status(StatusCommand::Next {
                number: positional_number(args, 1, "status next")?,
            }))
        }
        "set" => {
            validate_options(args, 1, &["--status"], &[], "status set")?;
            Ok(Command::Status(StatusCommand::Set {
                number: positional_number(args, 1, "status set")?,
                status: required_nonempty_option(args, "--status", "status set")?,
            }))
        }
        "advance" => {
            validate_options(args, 1, &["--status"], &[], "status advance")?;
            Ok(Command::Status(StatusCommand::Advance {
                number: positional_number(args, 1, "status advance")?,
                status: required_nonempty_option(args, "--status", "status advance")?,
            }))
        }
        value => Err(format!("unknown status command '{value}'")),
    }
}
