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
        Some("set") => parse_config_set(&args[1..]),
        Some("clear") => parse_config_clear(&args[1..]),
        Some("provider") => parse_config_provider(&args[1..]),
        Some(other) => Err(format!("unknown config command '{other}'")),
        None => Err("config requires a subcommand (show, set, clear, or provider)".to_owned()),
    }
}

fn parse_config_set(args: &[String]) -> Result<Command, String> {
    if args.is_empty() {
        return Err("config set requires a setting name".to_owned());
    }
    // Help already handled at top level for `config set --help` via parent check.
    // Still handle direct `config set --help` style where setting name is help flag.
    if args[0] == "--help" || args[0] == "-h" {
        return Ok(Command::Help(HelpTopic::ConfigCommand("set".to_owned())));
    }
    let setting_raw = &args[0];
    let canonical = crate::config_write::canonical_setting_name(setting_raw)
        .ok_or_else(|| format!("unknown config setting '{setting_raw}'"))?
        .to_owned();

    // Remaining tokens after setting: optional value and optional --stdin.
    let mut value: Option<String> = None;
    let mut stdin = false;
    for token in &args[1..] {
        if token == "--stdin" {
            if stdin {
                return Err("duplicate --stdin for config set".to_owned());
            }
            stdin = true;
        } else if token.starts_with('-') {
            return Err(format!("unknown option '{token}' for config set"));
        } else {
            if value.is_some() {
                return Err("config set takes at most one value".to_owned());
            }
            value = Some(token.clone());
        }
    }

    // Secret settings must never accept a direct value.
    if crate::config_write::is_secret_setting(&canonical) && value.is_some() {
        return Err(format!(
            "secret setting '{canonical}' does not accept a direct value; use --stdin or interactive prompt"
        ));
    }
    if stdin && value.is_some() {
        return Err("cannot provide both a value and --stdin".to_owned());
    }
    // For non-secret, require either value or --stdin.
    if !crate::config_write::is_secret_setting(&canonical) && value.is_none() && !stdin {
        return Err(format!(
            "config set {canonical} requires a value or --stdin"
        ));
    }

    Ok(Command::ConfigSet {
        setting: canonical,
        value,
        stdin,
    })
}

fn parse_config_clear(args: &[String]) -> Result<Command, String> {
    if args.is_empty() {
        return Err("config clear requires a setting name".to_owned());
    }
    if args[0] == "--help" || args[0] == "-h" {
        return Ok(Command::Help(HelpTopic::ConfigCommand("clear".to_owned())));
    }
    if args[0].starts_with('-') {
        return Err(format!("unknown option '{}' for config clear", args[0]));
    }
    if args.len() != 1 {
        // Check for flags
        if args.iter().skip(1).any(|v| v.starts_with('-')) {
            for token in &args[1..] {
                if token.starts_with('-') {
                    return Err(format!("unknown option '{token}' for config clear"));
                }
            }
        }
        return Err("config clear takes exactly one setting".to_owned());
    }
    let setting_raw = &args[0];
    let canonical = crate::config_write::canonical_setting_name(setting_raw)
        .ok_or_else(|| format!("unknown config setting '{setting_raw}'"))?
        .to_owned();
    // Reject --stdin for clear? Not supported.
    Ok(Command::ConfigClear { setting: canonical })
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
