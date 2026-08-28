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

pub(crate) fn parse_relation(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Relation));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::RelationCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 2, "relation list")?;
            Ok(Command::Relation(RelationCommand::List {
                issue: positional_number(args, 1, "relation list")?,
            }))
        }
        "create" => {
            validate_options(
                args,
                1,
                &["--to", "--type", "--delay"],
                &[],
                "relation create",
            )?;
            let issue = positional_number(args, 1, "relation create")?;
            let to = optional_option(args, "--to")
                .ok_or_else(|| "relation create requires --to".to_owned())?;
            let to = to
                .parse::<u64>()
                .map_err(|_| "relation create --to requires a numeric issue id".to_owned())?;
            let type_value = required_option(args, "--type", "relation create")?;
            // Strict canonical input only; inverse names are never accepted as
            // CLI input so a relation can never be created backwards.
            let relation_type = match RedmineRelationType::parse_input(&type_value) {
                Ok(relation_type) => relation_type,
                Err(error) => {
                    return Err(match error {
                        ForgejoError::Config(message) => message,
                        other => other.to_string(),
                    });
                }
            };
            let delay = match optional_option(args, "--delay") {
                Some(value) => Some(value.parse::<u64>().map_err(|_| {
                    "relation create --delay requires a non-negative integer".to_owned()
                })?),
                None => None,
            };
            Ok(Command::Relation(RelationCommand::Create {
                issue,
                to,
                relation_type,
                delay,
            }))
        }
        "delete" => {
            // `--issue <SOURCE_ISSUE_IID>` is required for GitLab
            // because the DELETE endpoint is scoped per source issue;
            // Redmine and Forgejo ignore the flag. Requiring the
            // option always keeps the GitLab dispatch honest; users
            // who target Redmine or Forgejo can still pass any
            // positive id (or zero, which the provider layer
            // surfaces as a structured config error if it lands on
            // GitLab by mistake).
            validate_options(args, 1, &["--issue"], &[], "relation delete")?;
            let relation_id = positional_number(args, 1, "relation delete")?;
            let issue = match optional_option(args, "--issue") {
                Some(value) => Some(value.parse::<u64>().map_err(|_| {
                    "relation delete --issue requires a positive integer".to_owned()
                })?),
                None => None,
            };
            Ok(Command::Relation(RelationCommand::Delete {
                relation_id,
                issue,
            }))
        }
        value => Err(format!("unknown relation command '{value}'")),
    }
}
