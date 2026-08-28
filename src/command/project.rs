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

pub(crate) fn parse_project(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Project));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::ProjectCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 1, "project list")?;
            Ok(Command::Project(ProjectCommand::List))
        }
        "create" => {
            validate_options(
                args,
                0,
                &["--name", "--identifier", "--description"],
                &["--confirm"],
                "project create",
            )?;
            if !has_flag(args, "--confirm") {
                return Err("project create requires --confirm".to_owned());
            }
            Ok(Command::Project(ProjectCommand::Create {
                name: required_nonempty_option(args, "--name", "project create")?,
                identifier: required_nonempty_option(args, "--identifier", "project create")?,
                description: optional_option(args, "--description"),
                confirmed: has_flag(args, "--confirm"),
            }))
        }
        value => Err(format!("unknown project command '{value}'")),
    }
}
