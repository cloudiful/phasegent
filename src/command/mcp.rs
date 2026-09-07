#[allow(unused_imports)]
use super::parse_helpers::{
    has_flag, optional_option, planning_options, positional_number, require_exact_positionals,
    required_nonempty_option, required_option, required_value, split_inline, validate_options,
};
#[allow(unused_imports)]
use super::{
    Command, CommentCommand, HelpTopic, Invocation, IssueCommand, McpCommand, McpTransport,
    NotifyCommand, PlanningOptions, ProjectCommand, RelationCommand, StatusCommand, TimerCommand,
    VersionCommand, WorkflowCommand,
};
#[allow(unused_imports)]
use crate::policy::Role;
#[allow(unused_imports)]
use crate::providers::ProviderKind;
#[allow(unused_imports)]
use crate::providers::api::ForgejoError;
#[allow(unused_imports)]
use crate::providers::redmine::model::RedmineRelationType;

pub(crate) fn parse_mcp(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Mcp));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::McpCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "serve" => {
            validate_options(
                args,
                0,
                &["--transport", "--bind"],
                &["--authorized"],
                "mcp serve",
            )?;
            let transport_raw =
                optional_option(args, "--transport").unwrap_or_else(|| "stdio".to_owned());
            let transport = match transport_raw.trim().to_ascii_lowercase().as_str() {
                "stdio" => McpTransport::Stdio,
                "http" => McpTransport::Http,
                _ => {
                    return Err("mcp serve --transport must be stdio or http".to_owned());
                }
            };
            let bind =
                optional_option(args, "--bind").unwrap_or_else(|| "127.0.0.1:3000".to_owned());
            if bind.trim().is_empty() {
                return Err("mcp serve --bind cannot be empty".to_owned());
            }
            if transport == McpTransport::Stdio && optional_option(args, "--bind").is_some() {
                return Err("mcp serve --bind requires --transport http".to_owned());
            }
            if transport == McpTransport::Http {
                bind.trim().parse::<std::net::SocketAddr>().map_err(|_| {
                    "mcp serve --bind must be a socket address like 127.0.0.1:3000".to_owned()
                })?;
            }
            let authorized = has_flag(args, "--authorized");
            Ok(Command::Mcp(McpCommand::Serve {
                transport,
                bind: bind.trim().to_owned(),
                authorized,
            }))
        }
        value => Err(format!("unknown mcp command '{value}'")),
    }
}
