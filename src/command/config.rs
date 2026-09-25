use super::prelude::*;

/// Which surface a `config` invocation was parsed from. The two origins share
/// the same argv grammar but not the same help pages: the top-level group is
/// read-only, while `admin config` is the human-operator write surface. Help
/// topics must be resolved from the origin so a command-form `--help` cannot
/// fall back to the other surface's page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigOrigin {
    /// `phasegent config ...` (read-only).
    TopLevel,
    /// `phasegent admin config ...` (human-operator writes).
    Admin,
}

impl ConfigOrigin {
    const fn group_topic(self) -> HelpTopic {
        match self {
            Self::TopLevel => HelpTopic::Config,
            Self::Admin => HelpTopic::AdminConfig,
        }
    }

    const fn provider_topic(self) -> HelpTopic {
        match self {
            Self::TopLevel => HelpTopic::ConfigProvider,
            Self::Admin => HelpTopic::AdminConfigProvider,
        }
    }
}

/// `admin config ...` — write-only provisioning surface. Reuses the shared
/// parser with the admin origin, then rejects the read-only views (which stay
/// top-level) so the group never becomes a backdoor read path and `--help`
/// routes to the admin write pages.
pub(crate) fn parse_config_admin(args: &[String]) -> Result<Command, String> {
    if args.is_empty() {
        return Err(
            "admin config requires a subcommand (set, clear, or provider set/clear)".to_owned(),
        );
    }
    match parse_config_for(args, ConfigOrigin::Admin)? {
        Command::ConfigShow => Err(
            "`config show` is read-only and stays top-level: run `phasegent config show`"
                .to_owned(),
        ),
        Command::ConfigProviderGet => Err(
            "`config provider get` is read-only and stays top-level: run `phasegent config provider get`"
                .to_owned(),
        ),
        command => Ok(command),
    }
}

/// Top-level `config` parser: the read-only origin.
pub(crate) fn parse_config(args: &[String]) -> Result<Command, String> {
    parse_config_for(args, ConfigOrigin::TopLevel)
}

fn parse_config_for(args: &[String], origin: ConfigOrigin) -> Result<Command, String> {
    if args
        .first()
        .is_some_and(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(origin.group_topic()));
    }
    let subcommand = args.first().map(String::as_str);
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        if let Some(subcommand) = subcommand {
            return Ok(Command::Help(config_detail_help(
                origin,
                subcommand,
                &args[1..],
            )));
        }
        return Ok(Command::Help(origin.group_topic()));
    }
    match subcommand {
        Some("show") => {
            // `config show` deliberately accepts no options: the
            // optional role filter lives on the top-level
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
        Some("provider") => parse_config_provider(&args[1..], origin),
        Some(other) => Err(format!("unknown config command '{other}'")),
        None => Err("config requires a subcommand (show, set, clear, or provider)".to_owned()),
    }
}

/// Resolve the help topic for one `config` subcommand from the origin that
/// supplied it. Read-only details keep their read-only topics; every write
/// detail maps to its admin-scoped topic so the help gate denies AI roles
/// before the page renders.
fn config_detail_help(origin: ConfigOrigin, subcommand: &str, rest: &[String]) -> HelpTopic {
    match subcommand {
        "show" => HelpTopic::ConfigCommand("show".to_owned()),
        "set" | "clear" => HelpTopic::AdminConfigCommand(subcommand.to_owned()),
        "provider" => match rest.first().map(String::as_str) {
            Some("get") => HelpTopic::ConfigProviderCommand("get".to_owned()),
            Some("set") => HelpTopic::AdminConfigProviderCommand("set".to_owned()),
            Some("clear") => HelpTopic::AdminConfigProviderCommand("clear".to_owned()),
            // Group help, or an unknown nested token that falls back to the
            // origin's group page.
            _ => origin.provider_topic(),
        },
        // Unknown subcommands fall back to the origin's group page rather than
        // leaking the other surface.
        _ => origin.group_topic(),
    }
}

fn parse_config_set(args: &[String]) -> Result<Command, String> {
    if args.is_empty() {
        return Err("config set requires a setting name".to_owned());
    }
    // `--help` normally lands on the origin-aware help branch in
    // `parse_config_for`; keep this for direct calls so the write detail page
    // is still admin-scoped.
    if args[0] == "--help" || args[0] == "-h" {
        return Ok(Command::Help(HelpTopic::AdminConfigCommand(
            "set".to_owned(),
        )));
    }
    let setting_raw = &args[0];
    let canonical = crate::config_write::canonical_setting_name(setting_raw)
        .ok_or_else(|| format!("unknown config setting '{setting_raw}'"))?
        .to_owned();

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
        return Ok(Command::Help(HelpTopic::AdminConfigCommand(
            "clear".to_owned(),
        )));
    }
    if args[0].starts_with('-') {
        return Err(format!("unknown option '{}' for config clear", args[0]));
    }
    if args.len() != 1 {
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
    Ok(Command::ConfigClear { setting: canonical })
}

/// Parse the `config provider` surface, which exposes the persisted
/// machine-wide default through `get`/`set`/`clear`. `get` needs no role
/// because the global default is machine-wide; `set`/`clear` are human-operator
/// writes and help must resolve to the admin-scoped topics from the admin
/// origin (and for the moved top-level write form).
fn parse_config_provider(args: &[String], origin: ConfigOrigin) -> Result<Command, String> {
    if args
        .first()
        .is_some_and(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(origin.provider_topic()));
    }
    let subcommand = args.first().map(String::as_str);
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(config_detail_help(origin, "provider", args)));
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
                    "config provider set takes exactly one argument (forgejo, redmine, gitlab, or local)"
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

#[cfg(test)]
#[path = "config_help_tests.rs"]
mod help_tests;
