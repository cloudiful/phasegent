//! Parser for the `plugin` command group (issue #239 Phase 3).
//!
//! Mirrors the `command/<name>.rs` pattern used by `hooks`,
//! `comment`, `worktree`, and others:
//!
//! * `args.first()` is the subcommand name (`install` / `status` /
//!   `uninstall`); a missing or `--help` first token returns
//!   `Command::Help(HelpTopic::Plugin)`, and a trailing `--help`
//!   returns `Command::Help(HelpTopic::PluginCommand(name))`.
//! * Each subcommand uses the standard `validate_options` /
//!   `optional_option` / `has_flag` helpers so the leading-dash
//!   escape hatch (`--option=VALUE`) works for values that begin
//!   with `-`.
//! * No role / provider / network is touched here; the role gate is
//!   not applied because plugin management is operator-local and
//!   mirrors the hooks install surface (no role required).

use super::parse_helpers::{has_flag, validate_options};
use super::{Command as RootCommand, HelpTopic, PluginCommand};

pub(crate) fn parse_plugin(args: &[String]) -> Result<RootCommand, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || matches!(name, Some("--help" | "-h")) {
        return Ok(RootCommand::Help(HelpTopic::Plugin));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(RootCommand::Help(HelpTopic::PluginCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "install" => parse_install(args),
        "status" => parse_status(args),
        "uninstall" => parse_uninstall(args),
        value => Err(format!("unknown plugin command '{value}'")),
    }
}

fn parse_install(args: &[String]) -> Result<RootCommand, String> {
    validate_options(
        args,
        0,
        &[],
        &["--global", "--project", "--force"],
        "plugin install",
    )?;
    let global = has_flag(args, "--global");
    let project = has_flag(args, "--project");
    let force = has_flag(args, "--force");
    Ok(RootCommand::Plugin(PluginCommand::Install {
        global,
        project,
        force,
    }))
}

fn parse_status(args: &[String]) -> Result<RootCommand, String> {
    validate_options(args, 0, &[], &[], "plugin status")?;
    Ok(RootCommand::Plugin(PluginCommand::Status))
}

fn parse_uninstall(args: &[String]) -> Result<RootCommand, String> {
    validate_options(args, 0, &[], &["--global", "--project"], "plugin uninstall")?;
    let global = has_flag(args, "--global");
    let project = has_flag(args, "--project");
    Ok(RootCommand::Plugin(PluginCommand::Uninstall {
        global,
        project,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::{Command, PluginCommand};
    use crate::command;

    fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
        values.into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn install_defaults_to_both_scopes_unforced() {
        let invocation =
            command::parse_with_role_env(&strings(["plugin", "install"]), Some("orchestrator"))
                .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Install {
                global,
                project,
                force,
            }) => {
                assert!(!global);
                assert!(!project);
                assert!(!force);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn install_parses_global_project_force_flags() {
        let invocation = command::parse_with_role_env(
            &strings(["plugin", "install", "--global", "--project", "--force"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Install {
                global,
                project,
                force,
            }) => {
                assert!(global);
                assert!(project);
                assert!(force);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn install_rejects_unknown_option() {
        let err = command::parse_with_role_env(
            &strings(["plugin", "install", "--bogus"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("unknown option"), "got: {err}");
    }

    #[test]
    fn status_parses_without_flags() {
        let invocation =
            command::parse_with_role_env(&strings(["plugin", "status"]), Some("orchestrator"))
                .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Status) => {}
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn status_rejects_unknown_option() {
        let err = command::parse_with_role_env(
            &strings(["plugin", "status", "--global"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("unknown option"), "got: {err}");
    }

    #[test]
    fn uninstall_defaults_to_both_scopes() {
        let invocation =
            command::parse_with_role_env(&strings(["plugin", "uninstall"]), Some("orchestrator"))
                .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Uninstall { global, project }) => {
                assert!(!global);
                assert!(!project);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn uninstall_parses_scope_flags() {
        let invocation = command::parse_with_role_env(
            &strings(["plugin", "uninstall", "--project"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Uninstall { global, project }) => {
                assert!(!global);
                assert!(project);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn plugin_install_does_not_require_role() {
        // `plugin install` mirrors `hooks install`: operator-local,
        // never touches a provider, so no role is required.
        let invocation = command::parse(&strings(["plugin", "install"])).unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Install { .. }) => {}
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn plugin_unknown_subcommand_is_rejected() {
        let err =
            command::parse_with_role_env(&strings(["plugin", "reinstall"]), Some("orchestrator"))
                .unwrap_err();
        assert!(err.contains("unknown plugin command"), "got: {err}");
    }
}
