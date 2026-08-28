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

pub(crate) fn help_topic(
    value: &str,
    subcommand: Option<&str>,
    nested_subcommand: Option<&str>,
) -> Result<HelpTopic, String> {
    match value {
        "issue" => match subcommand {
            None => Ok(HelpTopic::Issue),
            Some(value) if ["get", "search", "create", "update-body", "close"].contains(&value) => {
                Ok(HelpTopic::IssueCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown issue help topic '{value}'")),
        },
        "comment" => match subcommand {
            None => Ok(HelpTopic::Comment),
            Some(value) if ["create", "get", "find-marker"].contains(&value) => {
                Ok(HelpTopic::CommentCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown comment help topic '{value}'")),
        },
        "project" => match subcommand {
            None => Ok(HelpTopic::Project),
            Some(value) if ["list", "create"].contains(&value) => {
                Ok(HelpTopic::ProjectCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown project help topic '{value}'")),
        },
        "status" => match subcommand {
            None => Ok(HelpTopic::Status),
            Some("list") | Some("next") | Some("set") | Some("advance") => {
                Ok(HelpTopic::StatusCommand(subcommand.unwrap().to_owned()))
            }
            Some(value) => Err(format!("unknown status help topic '{value}'")),
        },
        "version" => match subcommand {
            None => Ok(HelpTopic::Version),
            Some("list") => Ok(HelpTopic::VersionCommand("list".to_owned())),
            Some(value) => Err(format!("unknown version help topic '{value}'")),
        },
        "relation" => match subcommand {
            None => Ok(HelpTopic::Relation),
            Some(value) if ["list", "create", "delete"].contains(&value) => {
                Ok(HelpTopic::RelationCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown relation help topic '{value}'")),
        },
        "timer" => match subcommand {
            None => Ok(HelpTopic::Timer),
            Some(value) if ["start", "finish", "list", "get", "recover"].contains(&value) => {
                Ok(HelpTopic::TimerCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown timer help topic '{value}'")),
        },
        "workflow" => match subcommand {
            None => Ok(HelpTopic::Workflow),
            Some("bootstrap") => Ok(HelpTopic::WorkflowCommand("bootstrap".to_owned())),
            Some(value) => Err(format!("unknown workflow help topic '{value}'")),
        },
        "auth" => Ok(HelpTopic::Auth),
        "config" => match subcommand {
            None => Ok(HelpTopic::Config),
            Some("show") | Some("import-env") => {
                Ok(HelpTopic::ConfigCommand(subcommand.unwrap().to_owned()))
            }
            Some("provider") => match nested_subcommand {
                None => Ok(HelpTopic::ConfigProvider),
                Some("get") | Some("set") | Some("clear") => Ok(HelpTopic::ConfigProviderCommand(
                    nested_subcommand.unwrap().to_owned(),
                )),
                Some(value) => Err(format!("unknown config provider help topic '{value}'")),
            },
            Some(value) => Err(format!("unknown config help topic '{value}'")),
        },
        "repo" => match subcommand {
            None => Ok(HelpTopic::Repo),
            Some("create") => Ok(HelpTopic::RepoCommand("create".to_owned())),
            Some(value) => Err(format!("unknown repo help topic '{value}'")),
        },
        "ci" => match (subcommand, nested_subcommand) {
            (None, _) => Ok(HelpTopic::Ci),
            (Some("runs"), None) | (Some("inspect"), None) => {
                Ok(HelpTopic::CiCommand(subcommand.unwrap().to_owned()))
            }
            (Some("run"), None) | (Some("job"), None) => {
                Ok(HelpTopic::CiCommand(subcommand.unwrap().to_owned()))
            }
            (Some("run"), Some(value)) if ["get", "jobs"].contains(&value) => {
                Ok(HelpTopic::CiCommand(format!("run {value}")))
            }
            (Some("job"), Some("logs")) => Ok(HelpTopic::CiCommand("job logs".to_owned())),
            (Some(value), _) => Err(format!("unknown ci help topic '{value}'")),
        },
        "hooks" => match subcommand {
            None => Ok(HelpTopic::Hooks),
            Some("install") => Ok(HelpTopic::HooksCommand("install".to_owned())),
            Some(value) => Err(format!("unknown hooks help topic '{value}'")),
        },
        _ => Err(format!("unknown help topic '{value}'")),
    }
}
