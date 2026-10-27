use super::global_options::with_global_option_hint;
use super::parse_helpers::{required_value, split_inline};
use super::{
    Command, HelpTopic, Invocation, IssueCommand, WorktreeCommand, admin, auth, comment, config,
    help_topic, hooks, issue, notify, plugin, project, record, registry, relation, status, timer,
    version, workflow, worktree,
};
use crate::output::context::{OutputMode, OutputOptions, OutputSelection};
use crate::policy::{CallerContext, Role};

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
    // Unix raw-value handling before any permissioned side effect:
    // session presence uses `var_os` (blank and non-Unicode count as
    // present); a non-Unicode role is an argument error, never absent.
    // Pure tests inject `&str` via `parse_outcome_with_caller_env` and stay
    // deterministic.
    parse_outcome_for_caller(args, caller_from_env()?)
}

/// Resolve the invocation caller from the process environment. The parser and
/// the presentation pre-scan share this resolution, so both agree on the same
/// caller context and neither re-decides a permission.
pub(crate) fn caller_from_env() -> Result<CallerContext, String> {
    let role = crate::policy::role_from_env()?;
    Ok(CallerContext::from_role_and_session(
        role,
        crate::policy::session_present(),
    ))
}

/// Resolve the invocation caller from an injected role value and the managed
/// session marker.
pub(crate) fn caller_from(
    role_env: Option<&str>,
    session_present: bool,
) -> Result<CallerContext, String> {
    let role = role_env
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<Role>()
                .map_err(|error| format!("PHASEGENT_ROLE is invalid: {error}"))
        })
        .transpose()?;
    Ok(CallerContext::from_role_and_session(role, session_present))
}

/// Role-injectable parser used by legacy unit tests. `role_env`
/// models `PHASEGENT_ROLE` so tests never mutate process-global state
/// (which would race the parallel parser assertions). Legacy tests assume
/// an unmanaged context (no managed session marker); pure caller-aware
/// tests use [`parse_outcome_with_caller_env`] with an explicit session flag.
#[allow(dead_code)]
pub(crate) fn parse_outcome_with_role_env(
    args: &[String],
    role_env: Option<&str>,
) -> Result<ParseOutcome, String> {
    parse_outcome_for_caller(args, caller_from(role_env, false)?)
}

/// Fully injectable parser: `role_env` models `PHASEGENT_ROLE` and
/// `session_present` models `PHASEGENT_SESSION_ID` presence (even blank).
/// Pure unit tests use this so parallel assertions never read global env.
#[allow(dead_code)] // used by the cfg(test) caller-injectable entry point
pub(crate) fn parse_outcome_with_caller_env(
    args: &[String],
    role_env: Option<&str>,
    session_present: bool,
) -> Result<ParseOutcome, String> {
    parse_outcome_for_caller(args, caller_from(role_env, session_present)?)
}

/// Parse the argv against an already resolved caller.
pub(crate) fn parse_outcome_for_caller(
    args: &[String],
    caller: CallerContext,
) -> Result<ParseOutcome, String> {
    // Role context comes only from `PHASEGENT_ROLE`, which a managed session
    // exports for its child processes. A blank value means "not provided", but
    // a non-empty invalid value is an error rather than a silent downgrade to
    // role-less execution. Session presence models `PHASEGENT_SESSION_ID`
    // (even blank counts as present) and selects unmanaged Operator vs
    // managed-missing fail-closed; it is never TTY detection and never an
    // identity claim.
    let role = caller.role();

    if args.is_empty() {
        return Ok(ParseOutcome::invocation(Invocation {
            role,
            caller,
            provider: None,
            api_base: None,
            repository: None,
            project_id: None,
            close_status_id: None,
            close_status_name: None,
            output: OutputOptions::default(),
            command: Command::Help(HelpTopic::Root),
        }));
    }

    let mut provider = None;
    let mut api_base = None;
    let mut repository = None;
    let mut project_id = None;
    let mut close_status_id = None;
    let mut close_status_name = None;
    let mut output_selection: Option<OutputSelection> = None;
    let mut no_progress = false;
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
                    caller,
                    provider,
                    api_base,
                    repository,
                    project_id,
                    close_status_id,
                    close_status_name,
                    output: OutputOptions {
                        selection: output_selection.unwrap_or(OutputSelection::Auto),
                        no_progress,
                        legacy: None,
                    },
                    command: Command::Help(topic),
                }));
            }
            "--version" | "-V" => {
                return Ok(ParseOutcome::invocation(Invocation {
                    role,
                    caller,
                    provider,
                    api_base,
                    repository,
                    project_id,
                    close_status_id,
                    close_status_name,
                    output: OutputOptions {
                        selection: output_selection.unwrap_or(OutputSelection::Auto),
                        no_progress,
                        legacy: None,
                    },
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
            "--output" => {
                let value = required_value(args, index, "--output")?;
                output_selection = Some(merge_output(
                    output_selection,
                    OutputSelection::parse(&value)?,
                )?);
                index += 2;
            }
            "--no-progress" => {
                no_progress = true;
                index += 1;
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
                if let Some(parsed) = split_inline(value, "--output") {
                    output_selection = Some(merge_output(
                        output_selection,
                        OutputSelection::parse(&parsed)?,
                    )?);
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
    // A legacy per-command format flag and an explicit global `--output` must
    // agree. The conflict is rejected here, at parse time, before any storage
    // or host work; an automatic `--output` defers to the legacy flag.
    let legacy = legacy_format_override(&command, rest);
    if let (Some(selection), Some(legacy)) = (output_selection, legacy)
        && selection != OutputSelection::Auto
    {
        let global = match selection {
            OutputSelection::Text => OutputMode::Text,
            OutputSelection::Json | OutputSelection::Auto => OutputMode::Json,
        };
        if global != legacy {
            return Err(format!(
                "worktree prune --format {} conflicts with --output {}",
                legacy.as_str(),
                selection.as_str()
            ));
        }
    }
    // Context-aware parser gate: an explicit Agent role consults the registry
    // as the single source of truth, so a denied command fails here with the
    // stable permission error instead of being accepted and rejected by the
    // execution layer after provider/credential setup. Unknown commands were
    // already rejected by `parse_command` with the distinct unknown error.
    // An unmanaged Operator parses every registered command (execution denies
    // only the agent-authored `record create` with a comment-create hint);
    // a managed session without a role fails closed for business commands
    // while the roleless utility allowlist stays available.
    match caller {
        CallerContext::Agent(role) => {
            if let Some(path) = super::command_registry_path(&command)
                && let Some(operation) = super::registry_denied_operation(role, &path)
            {
                return Ok(ParseOutcome::Permission { role, operation });
            }
        }
        CallerContext::Operator => {
            // No parser denial: Operator authority is checked by the
            // execution layer through the same context (record create is the
            // only denial, and it fires there before any provider access).
        }
        CallerContext::ManagedWithoutRole => {
            // Utility passthrough decided below; business fails here before
            // any credential/provider access.
        }
    }
    // Local branch context and hooks never touch provider credentials. The
    // internal `hooks run` forms are also invoked by generated Git scripts
    // without a role. `config set/clear` is allowed without a role when
    // the target is a global setting; role-scoped settings still require a
    // concrete scope (Operator uses the admin scope at execution time, a
    // managed session without a role fails closed).
    let no_role_allowed = match &command {
        Command::Help(_)
        | Command::Doctor
        | Command::ConfigShow
        | Command::ConfigProviderGet
        | Command::ConfigProviderSet { .. }
        | Command::ConfigProviderClear
        | Command::Hooks(_)
        | Command::Plugin(_)
        | Command::Issue(
            IssueCommand::Bind { .. } | IssueCommand::Unbind { .. } | IssueCommand::StatusBranch,
        ) => true,
        Command::ConfigSet { setting, .. } => crate::config_write::is_global_setting(setting),
        Command::ConfigClear { setting } => crate::config_write::is_global_setting(setting),
        _ => false,
    };
    match caller {
        CallerContext::Agent(_) => {}
        CallerContext::Operator => {}
        CallerContext::ManagedWithoutRole => {
            if !no_role_allowed {
                return Err(
                    "managed session requires a role for operations; set PHASEGENT_ROLE".to_owned(),
                );
            }
        }
    }
    if close_status_name.is_some() && !matches!(&command, Command::Workflow(_)) {
        return Err("--close-status-name is only supported by workflow bootstrap".to_owned());
    }
    Ok(ParseOutcome::invocation(Invocation {
        role,
        caller,
        provider,
        api_base,
        repository,
        project_id,
        close_status_id,
        close_status_name,
        output: OutputOptions {
            selection: output_selection.unwrap_or(OutputSelection::Auto),
            no_progress,
            legacy,
        },
        command,
    }))
}

/// A repeated `--output` is only accepted with the same value; a
/// contradictory duplicate is a parser error before any side effect.
fn merge_output(
    previous: Option<OutputSelection>,
    next: OutputSelection,
) -> Result<OutputSelection, String> {
    match previous {
        Some(previous) if previous != next => {
            Err("--output must not be given twice with different values".to_owned())
        }
        _ => Ok(next),
    }
}

/// The explicit legacy format of a parsed command, when the command carries
/// one. `worktree prune` keeps its `--json`/`--format text|json` flags: an
/// explicit legacy format wins over the automatic default, and a contradiction
/// with an explicit global `--output` is rejected at parse time.
fn legacy_format_override(command: &Command, rest: &[String]) -> Option<OutputMode> {
    match command {
        Command::Worktree(WorktreeCommand::Prune { format, .. }) => {
            // The parse-time default and an explicit `--format text` share the
            // same stored value, so the explicit request is recognized from
            // the raw flags of the subcommand.
            if !prune_legacy_flag_present(rest) {
                return None;
            }
            Some(match format.as_str() {
                "json" => OutputMode::Json,
                _ => OutputMode::Text,
            })
        }
        _ => None,
    }
}

/// Whether the prune invocation carried an explicit legacy format flag.
fn prune_legacy_flag_present(rest: &[String]) -> bool {
    rest.iter().any(|token| {
        token == "--json" || token == "--format" || split_inline(token, "--format").is_some()
    })
}

/// Role-injectable entry point for tests that assert the accepted invocation.
#[cfg(test)]
pub(crate) fn parse_with_role_env(
    args: &[String],
    role_env: Option<&str>,
) -> Result<Invocation, String> {
    parse_outcome_with_role_env(args, role_env).and_then(ParseOutcome::into_invocation)
}

/// Fully injectable entry point for deterministic caller tests: `role_env`
/// models `PHASEGENT_ROLE`, `session_present` models `PHASEGENT_SESSION_ID`
/// presence. Parallel tests use this instead of mutating process env.
#[cfg(test)]
pub(crate) fn parse_with_caller_env(
    args: &[String],
    role_env: Option<&str>,
    session_present: bool,
) -> Result<Invocation, String> {
    parse_outcome_with_caller_env(args, role_env, session_present)
        .and_then(ParseOutcome::into_invocation)
}

fn parse_command(command: &str, rest: &[String]) -> Result<Command, String> {
    // Top-level routing is registry-driven: a name without a descriptor is
    // unknown, so the registry can never fall behind the parser and accept a
    // command it does not describe.
    if registry::top_level(command).is_none() {
        return Err(format!("unknown command '{command}'"));
    }
    Ok(match command {
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
        "record" => record::parse_record(rest)?,
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
        "hooks" => hooks::parse_hooks(rest)?,
        "plugin" => plugin::parse_plugin(rest)?,
        "notify" => notify::parse_notify(rest)?,
        value => return Err(format!("unknown command '{value}'")),
    })
}

#[cfg(test)]
#[path = "argv_tests.rs"]
mod tests;
