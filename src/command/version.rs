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

pub(crate) fn parse_version(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Version));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::VersionCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 1, "version list")?;
            Ok(Command::VersionCommand(VersionCommand::List))
        }
        value => Err(format!("unknown version command '{value}'")),
    }
}
