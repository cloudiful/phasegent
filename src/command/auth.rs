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

pub(crate) fn parse_auth(args: &[String]) -> Result<Command, String> {
    if args
        .first()
        .is_some_and(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::Auth));
    }
    if args.first().map(String::as_str) != Some("setup") {
        return Err("auth requires the setup subcommand".to_owned());
    }
    let mut read_stdin = false;
    let mut provider = None;
    let mut api_base = None;
    let mut repository = None;
    let mut project_id = None;
    let mut close_status_id = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--stdin" => read_stdin = true,
            "--help" | "-h" => return Ok(Command::Help(HelpTopic::Auth)),
            "--provider" => {
                provider = Some(required_value(args, index, "--provider")?.parse()?);
                index += 1;
            }
            "--api-base" => {
                api_base = Some(required_value(args, index, "--api-base")?);
                index += 1;
            }
            "--repository" => {
                repository = Some(required_value(args, index, "--repository")?);
                index += 1;
            }
            "--project-id" => {
                project_id = Some(required_value(args, index, "--project-id")?);
                index += 1;
            }
            "--close-status-id" => {
                close_status_id = Some(required_value(args, index, "--close-status-id")?);
                index += 1;
            }
            value if value.starts_with("--") => {
                if let Some(parsed) = split_inline(value, "--provider") {
                    provider = Some(parsed.parse()?);
                } else if let Some(parsed) = split_inline(value, "--api-base") {
                    api_base = Some(parsed);
                } else if let Some(parsed) = split_inline(value, "--repository") {
                    repository = Some(parsed);
                } else if let Some(parsed) = split_inline(value, "--project-id") {
                    project_id = Some(parsed);
                } else if let Some(parsed) = split_inline(value, "--close-status-id") {
                    close_status_id = Some(parsed);
                } else {
                    return Err(format!("unknown auth setup option '{value}'"));
                }
            }
            value => return Err(format!("unknown auth setup option '{value}'")),
        }
        index += 1;
    }
    Ok(Command::AuthSetup {
        read_stdin,
        provider,
        api_base,
        repository,
        project_id,
        close_status_id,
    })
}
