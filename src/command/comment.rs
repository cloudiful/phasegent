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

pub(crate) fn parse_comment(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Comment));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::CommentCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "create" => {
            validate_options(
                args,
                1,
                &["--body", "--marker"],
                &["--authorized"],
                "comment create",
            )?;
            Ok(Command::Comment(CommentCommand::Create {
                issue: positional_number(args, 1, "comment create")?,
                body: required_option(args, "--body", "comment create")?,
                marker: required_nonempty_option(args, "--marker", "comment create")?,
                authorized: has_flag(args, "--authorized"),
            }))
        }
        "get" => {
            require_exact_positionals(args, 3, "comment get")?;
            Ok(Command::Comment(CommentCommand::Get {
                issue: positional_number(args, 1, "comment get")?,
                comment: positional_number(args, 2, "comment get")?,
            }))
        }
        "find-marker" => {
            validate_options(args, 1, &["--marker"], &[], "comment find-marker")?;
            Ok(Command::Comment(CommentCommand::FindMarker {
                issue: positional_number(args, 1, "comment find-marker")?,
                marker: required_nonempty_option(args, "--marker", "comment find-marker")?,
            }))
        }
        value => Err(format!("unknown comment command '{value}'")),
    }
}
