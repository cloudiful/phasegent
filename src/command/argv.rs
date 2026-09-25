use super::global_options::with_global_option_hint;
use super::parse_helpers::{required_value, split_inline};
use super::{
    Command, HelpTopic, Invocation, IssueCommand, admin, auth, comment, config, help_topic, hooks,
    issue, mcp, notify, plugin, project, registry, relation, status, timer, version, workflow,
    worktree,
};
use crate::policy::Role;

/// Parse the process argv, resolving the role from the `PHASEGENT_ROLE`
/// environment variable. A managed session exports that variable for its
/// child processes, so no CLI flag carries the role. Kept as a thin
/// wrapper so the environment lookup happens exactly once; the decision
/// logic lives in `parse_outcome`.
#[allow(dead_code)] // public parser entry used by the fuzz harness
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    parse_outcome(args).and_then(ParseOutcome::into_invocation)
}

/// The parser outcome: an accepted invocation, or a stable role denial for a
/// command the registry gates away. `cli::run` turns the latter into the same
/// structured `permission` error the execution layer would emit, so a denied
/// command fails before any provider or network access.
pub(crate) enum ParseOutcome {
    Invocation(Box<Invocation>),
    Permission { role: Role, operation: &'static str },
}

impl ParseOutcome {
    fn invocation(invocation: Invocation) -> Self {
        Self::Invocation(Box::new(invocation))
    }

    #[allow(dead_code)] // used by the cfg(test) role-injectable entry point
    fn into_invocation(self) -> Result<Invocation, String> {
        match self {
            Self::Invocation(invocation) => Ok(*invocation),
            Self::Permission { role, operation } => Err(permission_message(role, operation)),
        }
    }
}

/// The stable parser-level permission message, byte-identical to the
/// execution layer's generic denial.
pub(crate) fn permission_message(role: Role, operation: &str) -> String {
    format!("role '{role}' is not allowed to perform {operation}")
}

/// Parse the process argv with the process role environment.
pub(crate) fn parse_outcome(args: &[String]) -> Result<ParseOutcome, String> {
    let role_env = std::env::var("PHASEGENT_ROLE").ok();
    parse_outcome_with_role_env(args, role_env.as_deref())
}

/// Role-injectable parser used by `parse_outcome` and by unit tests. `role_env`
/// models `PHASEGENT_ROLE` so tests never mutate process-global state
/// (which would race the parallel parser assertions).
pub(crate) fn parse_outcome_with_role_env(
    args: &[String],
    role_env: Option<&str>,
) -> Result<ParseOutcome, String> {
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
        return Ok(ParseOutcome::invocation(Invocation {
            role,
            provider: None,
            api_base: None,
            repository: None,
            project_id: None,
            close_status_id: None,
            close_status_name: None,
            command: Command::Help(HelpTopic::Root),
        }));
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
                return Ok(ParseOutcome::invocation(Invocation {
                    role,
                    provider,
                    api_base,
                    repository,
                    project_id,
                    close_status_id,
                    close_status_name,
                    command: Command::Help(topic),
                }));
            }
            "--version" | "-V" => {
                return Ok(ParseOutcome::invocation(Invocation {
                    role,
                    provider,
                    api_base,
                    repository,
                    project_id,
                    close_status_id,
                    close_status_name,
                    command: Command::Version,
                }));
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
    // Role-aware parser gate: the registry is the single source of truth for
    // which command a role may run, so a denied command fails here with the
    // stable permission error instead of being accepted and rejected by the
    // execution layer after provider/credential setup. Unknown commands were
    // already rejected by `parse_command` with the distinct unknown error, and
    // a missing role keeps the historical superset routing (checked below).
    if let Some(role) = role
        && let Some(path) = super::command_registry_path(&command)
        && let Some(operation) = super::registry_denied_operation(role, &path)
    {
        return Ok(ParseOutcome::Permission { role, operation });
    }
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
    Ok(ParseOutcome::invocation(Invocation {
        role,
        provider,
        api_base,
        repository,
        project_id,
        close_status_id,
        close_status_name,
        command,
    }))
}

/// Role-injectable entry point for tests that assert the accepted invocation.
#[cfg(test)]
pub(crate) fn parse_with_role_env(
    args: &[String],
    role_env: Option<&str>,
) -> Result<Invocation, String> {
    parse_outcome_with_role_env(args, role_env).and_then(ParseOutcome::into_invocation)
}

fn parse_command(command: &str, rest: &[String]) -> Result<Command, String> {
    // Top-level routing is registry-driven: a name without a descriptor is
    // unknown, so the registry can never fall behind the parser and accept a
    // command it does not describe.
    if registry::top_level(command).is_none() {
        return Err(format!("unknown command '{command}'"));
    }
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
#[path = "argv_tests.rs"]
mod tests;
