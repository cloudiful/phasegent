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

pub(crate) fn parse_issue(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Issue));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::IssueCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "get" => {
            require_exact_positionals(args, 2, "issue get")?;
            Ok(Command::Issue(IssueCommand::Get {
                number: positional_number(args, 1, "issue get")?,
            }))
        }
        "search" => parse_issue_search(args),
        "create" => {
            validate_options(
                args,
                0,
                &[
                    "--title",
                    "--body",
                    "--tracker",
                    "--parent-issue",
                    "--fixed-version",
                    "--start-date",
                    "--due-date",
                    "--estimated-hours",
                    "--done-ratio",
                ],
                &[],
                "issue create",
            )?;
            Ok(Command::Issue(IssueCommand::Create {
                title: required_option(args, "--title", "issue create")?,
                body: optional_option(args, "--body").unwrap_or_default(),
                tracker: optional_option(args, "--tracker"),
                planning: planning_options(args),
            }))
        }
        "update-body" => {
            validate_options(
                args,
                1,
                &[
                    "--body",
                    "--tracker",
                    "--parent-issue",
                    "--fixed-version",
                    "--start-date",
                    "--due-date",
                    "--estimated-hours",
                    "--done-ratio",
                ],
                &[],
                "issue update-body",
            )?;
            Ok(Command::Issue(IssueCommand::UpdateBody {
                number: positional_number(args, 1, "issue update-body")?,
                body: required_option(args, "--body", "issue update-body")?,
                tracker: optional_option(args, "--tracker"),
                planning: planning_options(args),
            }))
        }
        "close" => {
            require_exact_positionals(args, 2, "issue close")?;
            Ok(Command::Issue(IssueCommand::Close {
                number: positional_number(args, 1, "issue close")?,
            }))
        }
        "bind" => {
            validate_options(args, 1, &[], &["--replace"], "issue bind")?;
            let issue_id = positional_number(args, 1, "issue bind")?;
            if issue_id == 0 {
                return Err("issue bind requires a positive issue id".to_owned());
            }
            Ok(Command::Issue(IssueCommand::Bind {
                issue_id,
                replace: has_flag(args, "--replace"),
            }))
        }
        "unbind" => {
            require_exact_positionals(args, 1, "issue unbind")?;
            Ok(Command::Issue(IssueCommand::Unbind))
        }
        // Local branch context status; unrelated to the provider-backed
        // top-level `status list` command.
        "status" => {
            require_exact_positionals(args, 1, "issue status")?;
            Ok(Command::Issue(IssueCommand::StatusBranch))
        }
        value => Err(format!("unknown issue command '{value}'")),
    }
}

fn parse_issue_search(args: &[String]) -> Result<Command, String> {
    validate_options(args, 0, &["--query", "-q", "--state"], &[], "issue search")?;
    let query = optional_option(args, "--query").or_else(|| optional_option(args, "-q"));
    let state = optional_option(args, "--state").unwrap_or_else(|| "all".to_owned());
    if !matches!(state.as_str(), "open" | "closed" | "all") {
        return Err("--state must be open, closed, or all".to_owned());
    }
    Ok(Command::Issue(IssueCommand::Search { query, state }))
}
