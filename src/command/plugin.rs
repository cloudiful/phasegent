//! Parser for the `plugin` command group (issue #239 Phase 3; issue #22 P2
//! scope cleanup).
//!
//! Mirrors the `command/<name>.rs` pattern used by `hooks`,
//! `comment`, `worktree`, and others:
//!
//! * `args.first()` is the subcommand name (`install` / `status` /
//!   `uninstall`); a missing or `--help` first token returns
//!   `Command::Help(HelpTopic::Plugin)`, and a trailing `--help`
//!   returns `Command::Help(HelpTopic::PluginCommand(name))`.
//! * Each subcommand uses the standard `validate_options` /
//!   `optional_nonempty_option` / `has_flag` helpers so the
//!   leading-dash escape hatch (`--path=VALUE`) works for values that
//!   begin with `-`.
//! * The default target is the global OpenCode plugin directory.
//!   `--project` selects only the project directory. `--path DIR` (issue
//!   666) names an explicit plugin directory — the directory holding
//!   `phasegent-worktree.js`, not the file itself — and is mutually
//!   exclusive with `--project`. A missing, empty, or whitespace-only value
//!   is a parser error rather than a silent fallback. `--global` was removed
//!   and is rejected as an unknown option.
//! * No role / provider / network is touched here; the role gate is
//!   not applied because plugin management is operator-local and
//!   mirrors the hooks install surface (no role required).

use super::parse_helpers::{has_flag, optional_nonempty_option, validate_options};
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
        &["--path"],
        &["--project", "--force"],
        "plugin install",
    )?;
    let project = has_flag(args, "--project");
    let force = has_flag(args, "--force");
    let path = parse_explicit_path(args, project, "plugin install")?;
    Ok(RootCommand::Plugin(PluginCommand::Install {
        project,
        force,
        path,
    }))
}

fn parse_status(args: &[String]) -> Result<RootCommand, String> {
    validate_options(args, 0, &["--path"], &["--project"], "plugin status")?;
    let project = has_flag(args, "--project");
    let path = parse_explicit_path(args, project, "plugin status")?;
    Ok(RootCommand::Plugin(PluginCommand::Status { project, path }))
}

fn parse_uninstall(args: &[String]) -> Result<RootCommand, String> {
    validate_options(args, 0, &["--path"], &["--project"], "plugin uninstall")?;
    let project = has_flag(args, "--project");
    let path = parse_explicit_path(args, project, "plugin uninstall")?;
    Ok(RootCommand::Plugin(PluginCommand::Uninstall {
        project,
        path,
    }))
}

/// Resolve `--path DIR` for a scope-bearing subcommand. The explicit
/// directory replaces the project selector, so combining them is a parser
/// error instead of an implicit precedence winner.
fn parse_explicit_path(
    args: &[String],
    project: bool,
    operation: &str,
) -> Result<Option<String>, String> {
    let path = optional_nonempty_option(args, "--path", operation)?;
    if path.is_some() && project {
        return Err(format!(
            "{operation} --path cannot be combined with --project"
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::super::{Command, PluginCommand};
    use crate::command;

    fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
        values.into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn install_defaults_to_the_global_target_unforced() {
        let invocation =
            command::parse_with_role_env(&strings(["plugin", "install"]), Some("orchestrator"))
                .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Install {
                project,
                force,
                path,
            }) => {
                assert!(!project);
                assert!(!force);
                assert_eq!(path, None);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn install_parses_project_and_force_flags() {
        let invocation = command::parse_with_role_env(
            &strings(["plugin", "install", "--project", "--force"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Install {
                project,
                force,
                path,
            }) => {
                assert!(project);
                assert!(force);
                assert_eq!(path, None);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn install_rejects_the_removed_global_flag() {
        let err = command::parse_with_role_env(
            &strings(["plugin", "install", "--global"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("unknown option '--global'"), "got: {err}");
    }

    #[test]
    fn install_parses_explicit_path() {
        let invocation = command::parse_with_role_env(
            &strings(["plugin", "install", "--path", "/tmp/chezmoi/plugins"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Install { project, path, .. }) => {
                assert!(!project);
                assert_eq!(path.as_deref(), Some("/tmp/chezmoi/plugins"));
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn install_parses_inline_path_escape_hatch() {
        let invocation = command::parse_with_role_env(
            &strings(["plugin", "install", "--path=-dashed-dir"]),
            Some("orchestrator"),
        )
        .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Install { path, .. }) => {
                assert_eq!(path.as_deref(), Some("-dashed-dir"));
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn install_rejects_path_combined_with_project() {
        let err = command::parse_with_role_env(
            &strings(["plugin", "install", "--path", "/tmp/plugins", "--project"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(err.contains("cannot be combined"), "got: {err}");
    }

    #[test]
    fn install_rejects_missing_or_blank_path_values() {
        for args in [
            strings(["plugin", "install", "--path"]),
            strings(["plugin", "install", "--path="]),
            strings(["plugin", "install", "--path", "   "]),
        ] {
            let err = command::parse_with_role_env(&args, Some("orchestrator")).unwrap_err();
            assert!(
                err.contains("requires a value") || err.contains("non-empty --path"),
                "got: {err}"
            );
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
    fn status_defaults_to_the_global_target() {
        let invocation =
            command::parse_with_role_env(&strings(["plugin", "status"]), Some("orchestrator"))
                .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Status { project, path }) => {
                assert!(!project);
                assert_eq!(path, None);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn status_parses_project_and_explicit_path() {
        let project = command::parse_with_role_env(
            &strings(["plugin", "status", "--project"]),
            Some("orchestrator"),
        )
        .unwrap();
        assert!(matches!(
            project.command,
            Command::Plugin(PluginCommand::Status {
                project: true,
                path: None
            })
        ));
        let path = command::parse_with_role_env(
            &strings(["plugin", "status", "--path", "/tmp/plugins"]),
            Some("orchestrator"),
        )
        .unwrap();
        assert!(matches!(
            path.command,
            Command::Plugin(PluginCommand::Status {
                project: false,
                path: Some(ref value)
            }) if value == "/tmp/plugins"
        ));
    }

    #[test]
    fn status_rejects_blank_path_and_removed_global() {
        let blank = command::parse_with_role_env(
            &strings(["plugin", "status", "--path="]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(blank.contains("non-empty --path"), "got: {blank}");
        let global = command::parse_with_role_env(
            &strings(["plugin", "status", "--global"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(
            global.contains("unknown option '--global'"),
            "got: {global}"
        );
    }

    #[test]
    fn uninstall_defaults_to_the_global_target() {
        let invocation =
            command::parse_with_role_env(&strings(["plugin", "uninstall"]), Some("orchestrator"))
                .unwrap();
        match invocation.command {
            Command::Plugin(PluginCommand::Uninstall { project, path }) => {
                assert!(!project);
                assert_eq!(path, None);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn uninstall_parses_project_and_explicit_path() {
        let project = command::parse_with_role_env(
            &strings(["plugin", "uninstall", "--project"]),
            Some("orchestrator"),
        )
        .unwrap();
        assert!(matches!(
            project.command,
            Command::Plugin(PluginCommand::Uninstall {
                project: true,
                path: None
            })
        ));
        let path = command::parse_with_role_env(
            &strings(["plugin", "uninstall", "--path", "/tmp/plugins"]),
            Some("orchestrator"),
        )
        .unwrap();
        assert!(matches!(
            path.command,
            Command::Plugin(PluginCommand::Uninstall {
                project: false,
                path: Some(ref value)
            }) if value == "/tmp/plugins"
        ));
    }

    #[test]
    fn uninstall_rejects_path_combined_with_project_and_removed_global() {
        let combined = command::parse_with_role_env(
            &strings(["plugin", "uninstall", "--path", "/tmp/plugins", "--project"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(combined.contains("cannot be combined"), "got: {combined}");
        let global = command::parse_with_role_env(
            &strings(["plugin", "uninstall", "--global"]),
            Some("orchestrator"),
        )
        .unwrap_err();
        assert!(
            global.contains("unknown option '--global'"),
            "got: {global}"
        );
    }

    #[test]
    fn plugin_install_does_not_require_role() {
        let invocation =
            command::parse_with_role_env(&strings(["plugin", "install"]), None).unwrap();
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
