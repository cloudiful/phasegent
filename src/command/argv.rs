use super::global_options::with_global_option_hint;
use super::parse_helpers::{required_value, split_inline};
use super::{
    Command, HelpTopic, Invocation, IssueCommand, admin, auth, comment, config, help_topic, hooks,
    issue, mcp, notify, plugin, project, relation, status, timer, version, workflow, worktree,
};
use crate::policy::Role;

/// Parse the process argv, resolving the role from the `PHASEGENT_ROLE`
/// environment variable. A managed session exports that variable for its
/// child processes, so no CLI flag carries the role. Kept as a thin
/// wrapper so the environment lookup happens exactly once; the decision
/// logic lives in `parse_with_role_env`.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    let role_env = std::env::var("PHASEGENT_ROLE").ok();
    parse_with_role_env(args, role_env.as_deref())
}

/// Role-injectable parser used by `parse` and by unit tests. `role_env`
/// models `PHASEGENT_ROLE` so tests never mutate process-global state
/// (which would race the parallel parser assertions).
pub(crate) fn parse_with_role_env(
    args: &[String],
    role_env: Option<&str>,
) -> Result<Invocation, String> {
    // Role context comes only from `PHASEGENT_ROLE`, which a managed session
    // exports for its child processes. A blank value means "not provided", but
    // a non-empty invalid value is an error rather than a silent downgrade to
    // role-less execution.
    let role = role_env
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<Role>()
                .map_err(|error| format!("PHASEGENT_ROLE is invalid: {error}"))
        })
        .transpose()?;

    if args.is_empty() {
        return Ok(Invocation {
            role,
            provider: None,
            api_base: None,
            repository: None,
            project_id: None,
            close_status_id: None,
            close_status_name: None,
            command: Command::Help(HelpTopic::Root),
        });
    }

    let mut provider = None;
    let mut api_base = None;
    let mut repository = None;
    let mut project_id = None;
    let mut close_status_id = None;
    let mut close_status_name = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--help" | "-h" => {
                let topic = args.get(index + 1).map_or(Ok(HelpTopic::Root), |value| {
                    help_topic::help_topic(
                        value,
                        args.get(index + 2).map(String::as_str),
                        args.get(index + 3).map(String::as_str),
                    )
                })?;
                return Ok(Invocation {
                    role,
                    provider,
                    api_base,
                    repository,
                    project_id,
                    close_status_id,
                    close_status_name,
                    command: Command::Help(topic),
                });
            }
            "--version" | "-V" => {
                return Ok(Invocation {
                    role,
                    provider,
                    api_base,
                    repository,
                    project_id,
                    close_status_id,
                    close_status_name,
                    command: Command::Version,
                });
            }
            "--provider" => {
                provider = Some(required_value(args, index, "--provider")?.parse()?);
                index += 2;
            }
            "--api-base" => {
                api_base = Some(required_value(args, index, "--api-base")?);
                index += 2;
            }
            "--repository" => {
                repository = Some(required_value(args, index, "--repository")?);
                index += 2;
            }
            "--project-id" => {
                project_id = Some(required_value(args, index, "--project-id")?);
                index += 2;
            }
            "--close-status-id" => {
                close_status_id = Some(required_value(args, index, "--close-status-id")?);
                index += 2;
            }
            "--close-status-name" => {
                close_status_name = Some(required_value(args, index, "--close-status-name")?);
                index += 2;
            }
            // Inline `--option=value` form is accepted as an escape hatch for values
            // that begin with `-` (Markdown bullets, separator lines, negative lookups).
            // The two-arg `--option value` form keeps strict missing-value detection
            // (next token starting with `-` still counts as missing).
            value if value.starts_with("--") => {
                if let Some(parsed) = split_inline(value, "--provider") {
                    provider = Some(parsed.parse()?);
                    index += 1;
                    continue;
                }
                if let Some(parsed) = split_inline(value, "--api-base") {
                    api_base = Some(parsed);
                    index += 1;
                    continue;
                }
                if let Some(parsed) = split_inline(value, "--repository") {
                    repository = Some(parsed);
                    index += 1;
                    continue;
                }
                if let Some(parsed) = split_inline(value, "--project-id") {
                    project_id = Some(parsed);
                    index += 1;
                    continue;
                }
                if let Some(parsed) = split_inline(value, "--close-status-id") {
                    close_status_id = Some(parsed);
                    index += 1;
                    continue;
                }
                if let Some(parsed) = split_inline(value, "--close-status-name") {
                    close_status_name = Some(parsed);
                    index += 1;
                    continue;
                }
                return Err(format!("unknown option '{value}'"));
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ => break,
        }
    }

    let command = args.get(index).ok_or("a command is required")?;
    let rest = &args[index + 1..];
    let command = parse_command(command, rest).map_err(with_global_option_hint)?;
    // Local branch context and hooks never touch provider credentials. The
    // internal `hooks run` forms are also invoked by generated Git scripts
    // without a role. `config set/clear` is allowed without a role when
    // the target is a global setting; role-scoped settings still require it.
    let no_role_allowed = match &command {
        Command::Help(_)
        | Command::Gui
        | Command::Doctor
        | Command::ConfigShow
        | Command::ConfigProviderGet
        | Command::ConfigProviderSet { .. }
        | Command::ConfigProviderClear
        | Command::Hooks(_)
        | Command::Plugin(_)
        | Command::Issue(
            IssueCommand::Bind { .. } | IssueCommand::Unbind | IssueCommand::StatusBranch,
        ) => true,
        Command::ConfigSet { setting, .. } => crate::config_write::is_global_setting(setting),
        Command::ConfigClear { setting } => crate::config_write::is_global_setting(setting),
        _ => false,
    };
    if role.is_none() && !no_role_allowed {
        return Err(
            "a role is required for operations; set PHASEGENT_ROLE or run in a managed session"
                .to_owned(),
        );
    }
    if close_status_name.is_some() && !matches!(&command, Command::Workflow(_)) {
        return Err("--close-status-name is only supported by workflow bootstrap".to_owned());
    }
    Ok(Invocation {
        role,
        provider,
        api_base,
        repository,
        project_id,
        close_status_id,
        close_status_name,
        command,
    })
}

fn parse_command(command: &str, rest: &[String]) -> Result<Command, String> {
    Ok(match command {
        "gui" => {
            if !rest.is_empty() {
                return Err("gui takes no arguments".to_owned());
            }
            Command::Gui
        }
        "doctor" => {
            if !rest.is_empty() {
                return Err("doctor takes no arguments".to_owned());
            }
            Command::Doctor
        }
        "admin" => admin::parse_admin(rest)?,
        "auth" => match auth::parse_auth(rest)? {
            help @ Command::Help(_) => help,
            _ => {
                return Err(admin::moved_error("auth setup", "admin auth setup"));
            }
        },
        "config" => {
            let parsed = config::parse_config(rest)?;
            match parsed {
                Command::Help(_) | Command::ConfigShow | Command::ConfigProviderGet => parsed,
                _ => {
                    return Err(admin::moved_error(
                        "config set/clear and config provider set/clear",
                        "admin config ...",
                    ));
                }
            }
        }
        "issue" => issue::parse_issue(rest)?,
        "comment" => comment::parse_comment(rest)?,
        "project" => project::parse_project(rest)?,
        "status" => status::parse_status(rest)?,
        "version" => version::parse_version(rest)?,
        "relation" => relation::parse_relation(rest)?,
        "timer" => timer::parse_timer(rest)?,
        "workflow" => match workflow::parse_workflow(rest)? {
            help @ Command::Help(_) => help,
            _ => {
                return Err(admin::moved_error(
                    "workflow bootstrap",
                    "admin workflow bootstrap",
                ));
            }
        },
        "worktree" => worktree::parse_worktree(rest)?,
        "repo" => crate::repo_command::parse(rest)?,
        "hooks" => hooks::parse_hooks(rest)?,
        "plugin" => plugin::parse_plugin(rest)?,
        "notify" => notify::parse_notify(rest)?,
        "mcp" => mcp::parse_mcp(rest)?,
        value => return Err(format!("unknown command '{value}'")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn removed_role_flag_is_rejected_as_an_unknown_option() {
        for token in ["--role", "--role=executor"] {
            let error = parse_with_role_env(
                &args(&[token, "executor", "issue", "get", "1"]),
                Some("executor"),
            )
            .expect_err("the removed --role flag must be rejected");
            assert!(
                error.starts_with("unknown option '--role"),
                "token {token}: {error}"
            );
        }
    }

    #[test]
    fn misplaced_global_option_after_subcommand_hints_at_position() {
        // The reported flow: `--project-id` after `issue create` failed with a
        // bare "unknown option". The error must now state that global options
        // come before the subcommand and show a correct example.
        let error = parse_with_role_env(
            &args(&[
                "issue",
                "create",
                "--title",
                "T",
                "--body",
                "B",
                "--project-id",
                "23",
            ]),
            Some("orchestrator"),
        )
        .expect_err("a misplaced global option must be rejected");
        assert!(
            error.starts_with("unknown option '--project-id'"),
            "got: {error}"
        );
        assert!(
            error.contains("must come before the subcommand"),
            "got: {error}"
        );
        assert!(
            error.contains("--project-id 23 issue create"),
            "got: {error}"
        );
    }

    #[test]
    fn global_option_hint_covers_inline_and_separator_variants() {
        for token in ["--project-id=23", "--project_id"] {
            let error = parse_with_role_env(
                &args(&["issue", "close", "42", token]),
                Some("orchestrator"),
            )
            .expect_err("a misplaced global option must be rejected");
            assert!(
                error.contains("must come before the subcommand"),
                "token {token}: {error}"
            );
        }
    }

    #[test]
    fn unrelated_unknown_option_keeps_its_plain_message() {
        let error = parse_with_role_env(
            &args(&[
                "issue",
                "create",
                "--title",
                "T",
                "--body",
                "B",
                "--nonsense",
                "alpha",
            ]),
            Some("orchestrator"),
        )
        .expect_err("an unknown option must be rejected");
        assert_eq!(error, "unknown option '--nonsense'");
    }

    #[test]
    fn global_option_hint_only_matches_global_options() {
        for token in [
            "--provider",
            "--api-base",
            "--repository",
            "--project-id",
            "--close-status-id",
        ] {
            let error = parse_with_role_env(
                &args(&["issue", "bind", "42", token, "value"]),
                Some("orchestrator"),
            )
            .expect_err("a misplaced global option must be rejected");
            assert!(
                error.contains("must come before the subcommand"),
                "token {token}: {error}"
            );
        }
    }
}
