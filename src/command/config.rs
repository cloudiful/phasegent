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

pub(crate) fn parse_config(args: &[String]) -> Result<Command, String> {
    if args
        .first()
        .is_some_and(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::Config));
    }
    let subcommand = args.first().map(String::as_str);
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        if let Some(subcommand) = subcommand {
            return Ok(Command::Help(HelpTopic::ConfigCommand(
                subcommand.to_owned(),
            )));
        }
        return Ok(Command::Help(HelpTopic::Config));
    }
    match subcommand {
        Some("show") => {
            // `config show` deliberately accepts no options: the
            // optional `--role` filter lives on the top-level
            // invocation, which already routes `role` into
            // `ConfigShow`. Surplus arguments therefore indicate
            // the operator meant something else.
            if args.len() != 1 {
                return Err("config show takes no arguments".to_owned());
            }
            Ok(Command::ConfigShow)
        }
        Some("import-env") => {
            if args.len() != 1 {
                return Err(
                    "config import-env takes no arguments (--role selects the target row)"
                        .to_owned(),
                );
            }
            // Role is supplied via the outer `--role` flag; the CLI
            // layer turns `invocation.role` into the concrete
            // `Role` before dispatching. The outer parser already
            // rejects a missing `--role` because this command is not
            // one of the no-role-allowed commands (`Help`,
            // `ConfigShow`).
            Ok(Command::ConfigImportEnv)
        }
        Some("provider") => parse_config_provider(&args[1..]),
        Some(other) => Err(format!("unknown config command '{other}'")),
        None => Err("config requires a subcommand (show, import-env, or provider)".to_owned()),
    }
}

/// Parse the `config provider` surface, which exposes the persisted
/// machine-wide default through `get`/`set`/`clear`. None of these
/// commands require `--role` because the global default is, by
/// definition, machine-wide rather than role-scoped.

fn parse_config_provider(args: &[String]) -> Result<Command, String> {
    if args
        .first()
        .is_some_and(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::ConfigProvider));
    }
    let subcommand = args.first().map(String::as_str);
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        if let Some(subcommand) = subcommand {
            return Ok(Command::Help(HelpTopic::ConfigProviderCommand(
                subcommand.to_owned(),
            )));
        }
        return Ok(Command::Help(HelpTopic::ConfigProvider));
    }
    match subcommand {
        Some("get") => {
            if args.len() != 1 {
                return Err("config provider get takes no arguments".to_owned());
            }
            Ok(Command::ConfigProviderGet)
        }
        Some("set") => {
            if args.len() != 2 {
                return Err(
                    "config provider set takes exactly one argument (forgejo, redmine, or gitlab)"
                        .to_owned(),
                );
            }
            let value: ProviderKind = args[1]
                .parse()
                .map_err(|error: String| format!("config provider set: {error}"))?;
            Ok(Command::ConfigProviderSet { value })
        }
        Some("clear") => {
            if args.len() != 1 {
                return Err("config provider clear takes no arguments".to_owned());
            }
            Ok(Command::ConfigProviderClear)
        }
        Some(other) => Err(format!("unknown config provider command '{other}'")),
        None => Err("config provider requires a subcommand (get, set, or clear)".to_owned()),
    }
}
