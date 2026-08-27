use crate::forgejo_model::ForgejoError;
use crate::policy::Role;
use crate::provider::ProviderKind;
use crate::redmine_model::RedmineRelationType;

pub use crate::ci_command::CiCommand;
pub use crate::hooks::HooksCommand;
pub use crate::repo_command::RepoCommand;

#[derive(Debug)]
pub struct Invocation {
    pub role: Option<Role>,
    pub provider: Option<ProviderKind>,
    pub api_base: Option<String>,
    pub repository: Option<String>,
    pub project_id: Option<String>,
    pub close_status_id: Option<String>,
    pub close_status_name: Option<String>,
    pub command: Command,
}

#[derive(Debug)]
pub enum Command {
    Help(HelpTopic),
    Version,
    AuthSetup {
        read_stdin: bool,
        provider: Option<ProviderKind>,
        api_base: Option<String>,
        repository: Option<String>,
        project_id: Option<String>,
        close_status_id: Option<String>,
    },
    /// `config show` — redacted snapshot of the local SQLite
    /// database. The role filter (when supplied) comes from
    /// `Invocation::role`; the command is intentionally usable
    /// without `--role` so an operator can inspect the global view
    /// of configuration state.
    ConfigShow,
    /// `--role <ROLE> config import-env` — persist every
    /// role-scoped and global `PHASEGENT_*` environment variable
    /// that is currently set. Always requires a role because the
    /// role-scoped fields need a target row in `role_config` /
    /// `role_redmine_config`. Secret values are never echoed;
    /// callers receive counts plus per-name flags. The `Role`
    /// flows in from `Invocation::role` so the parser does not
    /// duplicate the missing-role check.
    ConfigImportEnv,
    /// `config provider get` — print the persisted
    /// `PHASEGENT_DEFAULT_PROVIDER` (machine-wide default) as
    /// JSON; `null` when unset. Usable without `--role` because
    /// the default is global, not role-scoped.
    ConfigProviderGet,
    /// `config provider set <forgejo|redmine|gitlab>` — validate
    /// the value through `ProviderKind::from_str` and persist it
    /// as the machine-wide default. Usable without `--role` for
    /// the same reason as `get`.
    ConfigProviderSet {
        value: ProviderKind,
    },
    /// `config provider clear` — remove the persisted
    /// `PHASEGENT_DEFAULT_PROVIDER` row so the resolver falls
    /// back to the role-scoped provider. Usable without `--role`.
    ConfigProviderClear,
    Issue(IssueCommand),
    Comment(CommentCommand),
    Project(ProjectCommand),
    Status(StatusCommand),
    /// Redmine project version operations; named to stay distinct from the
    /// pre-existing `Command::Version` (`--version`) variant.
    VersionCommand(VersionCommand),
    Workflow(WorkflowCommand),
    Repo(RepoCommand),
    Ci(CiCommand),
    Hooks(HooksCommand),
    /// Redmine issue relations. List, create, and delete by id; the create
    /// direction is `issue` -> `to` with a canonical `--type`.
    Relation(RelationCommand),
    /// Orchestrator-owned local phase timer and Redmine Time Entry
    /// projection. The child executor/reviewer roles do not call this CLI.
    Timer(TimerCommand),
}

#[derive(Debug)]
pub enum HelpTopic {
    Root,
    Issue,
    Comment,
    Project,
    Status,
    Auth,
    Config,
    ConfigCommand(String),
    ConfigProvider,
    ConfigProviderCommand(String),
    Repo,
    IssueCommand(String),
    CommentCommand(String),
    ProjectCommand(String),
    StatusCommand(String),
    Version,
    VersionCommand(String),
    Workflow,
    WorkflowCommand(String),
    RepoCommand(String),
    Ci,
    CiCommand(String),
    Hooks,
    HooksCommand(String),
    Relation,
    RelationCommand(String),
    Timer,
    TimerCommand(String),
}

#[derive(Debug)]
pub enum IssueCommand {
    Get {
        number: u64,
    },
    Search {
        query: Option<String>,
        state: String,
    },
    Create {
        title: String,
        body: String,
        /// Optional Redmine tracker selector (validated name or id) resolved
        /// against `/trackers.json` at execution time.
        tracker: Option<String>,
        /// Optional native Redmine planning fields; raw values are
        /// validated and version-resolved at execution time.
        planning: PlanningOptions,
    },
    UpdateBody {
        number: u64,
        body: String,
        /// Optional Redmine tracker re-selection applied in the same PUT.
        tracker: Option<String>,
        /// Optional native Redmine planning fields applied in the same PUT.
        planning: PlanningOptions,
    },
    Close {
        number: u64,
    },
    /// Local branch context operations (no provider access). `bind`
    /// stores the issue id under `branch.<name>.redmine-issue-id` in the
    /// local Git config and rejects a different existing binding unless
    /// `replace` is explicit.
    Bind {
        issue_id: u64,
        replace: bool,
    },
    Unbind,
    StatusBranch,
}

#[derive(Debug)]
pub enum CommentCommand {
    Create {
        issue: u64,
        body: String,
        marker: String,
        authorized: bool,
    },
    Get {
        issue: u64,
        comment: u64,
    },
    FindMarker {
        issue: u64,
        marker: String,
    },
}

#[derive(Debug)]
pub enum ProjectCommand {
    List,
    Create {
        name: String,
        identifier: String,
        description: Option<String>,
        confirmed: bool,
    },
}

#[derive(Debug)]
pub enum StatusCommand {
    List,
    /// Redmine-only read of the current status plus the policy-allowed
    /// next statuses. Available to every role that can read statuses.
    Next {
        number: u64,
    },
    /// Redmine-only status transition by validated status name or id.
    /// Orchestrator-only at execution time.
    Set {
        number: u64,
        status: String,
    },
    /// Redmine-only policy-preflighted transition: idempotent for the
    /// same status, rejected before any write for a canonical illegal
    /// edge, advisory for custom statuses. Orchestrator-only.
    Advance {
        number: u64,
        status: String,
    },
}

/// Raw planning option values captured by the parser. Numeric ranges,
/// date shapes, and `--fixed-version` resolution are validated at
/// execution time in `redmine_planning_cli` so the parser stays purely
/// structural.
#[derive(Debug, Default)]
pub struct PlanningOptions {
    pub parent_issue: Option<String>,
    pub fixed_version: Option<String>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub estimated_hours: Option<String>,
    pub done_ratio: Option<String>,
}

impl PlanningOptions {
    /// True when no planning flag was supplied; such invocations keep the
    /// exact legacy execution path and payload.
    pub fn is_empty(&self) -> bool {
        self.parent_issue.is_none()
            && self.fixed_version.is_none()
            && self.start_date.is_none()
            && self.due_date.is_none()
            && self.estimated_hours.is_none()
            && self.done_ratio.is_none()
    }
}

#[derive(Debug)]
pub enum VersionCommand {
    List,
}

#[derive(Debug)]
pub enum RelationCommand {
    List {
        issue: u64,
    },
    Create {
        issue: u64,
        to: u64,
        relation_type: RedmineRelationType,
        delay: Option<u64>,
    },
    Delete {
        relation_id: u64,
        /// Optional source issue iid. Required for GitLab because the
        /// DELETE endpoint is scoped per source issue; Redmine and
        /// Forgejo ignore the field. Carrying it on the shared enum
        /// keeps the GitLab dispatch backward-compatible without
        /// silently guessing the source.
        issue: Option<u64>,
    },
}

#[derive(Debug)]
pub enum TimerCommand {
    Start {
        issue: u64,
        phase: String,
        agent_role: String,
        attempt: u64,
        run_id: Option<String>,
        owner_session_id: Option<String>,
        owner_call_id: Option<String>,
    },
    Finish {
        run_id: String,
        result: String,
    },
    /// Read-only listing of execution-ledger rows. `status_filter` selects
    /// the row subset (running, finished, or all); defaults to `all`.
    List {
        status: String,
        limit: u32,
    },
    /// Read-only inspection of a single row. Returns a structured error
    /// when the run id is unknown; never mutates state.
    Get {
        run_id: String,
    },
    /// Explicit `FAILED` recovery for a known orphan. Equivalent to a
    /// user-authorized `timer finish --result FAILED` and then a same-run
    /// provider projection (with `sync_status` reconciliation). If the
    /// row is already terminal, the operation is rejected so a recovered
    /// run cannot overwrite a previous outcome.
    Recover {
        run_id: String,
    },
}

#[derive(Debug)]
pub enum WorkflowCommand {
    Bootstrap {
        repository: Option<String>,
        close_status_id: Option<String>,
        close_status_name: Option<String>,
    },
}

pub fn parse(args: &[String]) -> Result<Invocation, String> {
    if args.is_empty() {
        return Ok(Invocation {
            role: None,
            provider: None,
            api_base: None,
            repository: None,
            project_id: None,
            close_status_id: None,
            close_status_name: None,
            command: Command::Help(HelpTopic::Root),
        });
    }

    let mut role = None;
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
                    help_topic(
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
            "--role" => {
                role = Some(required_value(args, index, "--role")?.parse()?);
                index += 2;
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
                if let Some(parsed) = split_inline(value, "--role") {
                    role = Some(parsed.parse()?);
                    index += 1;
                    continue;
                }
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
    let command = match command.as_str() {
        "auth" => parse_auth(rest)?,
        "config" => parse_config(rest)?,
        "issue" => parse_issue(rest)?,
        "comment" => parse_comment(rest)?,
        "project" => parse_project(rest)?,
        "status" => parse_status(rest)?,
        "version" => parse_version(rest)?,
        "relation" => parse_relation(rest)?,
        "timer" => parse_timer(rest)?,
        "workflow" => parse_workflow(rest)?,
        "repo" => crate::repo_command::parse(rest)?,
        "ci" => crate::ci_command::parse(rest)?,
        "hooks" => parse_hooks(rest)?,
        value => return Err(format!("unknown command '{value}'")),
    };
    // Local branch context and hooks never touch provider credentials. The
    // internal `hooks run` forms are also invoked by generated Git scripts
    // without a role.
    if role.is_none()
        && !matches!(
            &command,
            Command::Help(_)
                | Command::ConfigShow
                | Command::ConfigProviderGet
                | Command::ConfigProviderSet { .. }
                | Command::ConfigProviderClear
                | Command::Hooks(_)
                | Command::Issue(
                    IssueCommand::Bind { .. } | IssueCommand::Unbind | IssueCommand::StatusBranch
                )
        )
    {
        return Err("--role is required for operations".to_owned());
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

fn parse_timer(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Timer));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::TimerCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "start" => {
            validate_options(
                args,
                1,
                &[
                    "--phase",
                    "--agent-role",
                    "--attempt",
                    "--run-id",
                    "--owner-session-id",
                    "--owner-call-id",
                ],
                &[],
                "timer start",
            )?;
            let issue = positional_number(args, 1, "timer start")?;
            if issue == 0 {
                return Err("timer start requires a positive issue id".to_owned());
            }
            let phase = required_nonempty_option(args, "--phase", "timer start")?;
            let agent_role = required_nonempty_option(args, "--agent-role", "timer start")?
                .parse::<Role>()
                .map_err(|error| format!("timer start --agent-role: {error}"))?;
            if !matches!(agent_role, Role::Executor | Role::Reviewer) {
                return Err("timer start --agent-role must be executor or reviewer".to_owned());
            }
            let attempt = required_option(args, "--attempt", "timer start")?
                .parse::<u64>()
                .map_err(|_| "timer start --attempt requires a non-negative integer".to_owned())?;
            if attempt == 0 {
                return Err("timer start --attempt must be greater than zero".to_owned());
            }
            let run_id = optional_option(args, "--run-id");
            if run_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err("timer start --run-id cannot be empty".to_owned());
            }
            let owner_session_id = optional_option(args, "--owner-session-id");
            if owner_session_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err("timer start --owner-session-id cannot be empty".to_owned());
            }
            let owner_call_id = optional_option(args, "--owner-call-id");
            if owner_call_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err("timer start --owner-call-id cannot be empty".to_owned());
            }
            Ok(Command::Timer(TimerCommand::Start {
                issue,
                phase,
                agent_role: agent_role.as_str().to_owned(),
                attempt,
                run_id,
                owner_session_id,
                owner_call_id,
            }))
        }
        "finish" => {
            validate_options(args, 1, &["--result"], &[], "timer finish")?;
            let run_id = args
                .get(1)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or_else(|| "timer finish run id cannot be empty".to_owned())?;
            let result = required_option(args, "--result", "timer finish")?;
            if !["DONE", "PARTIAL", "BLOCKED", "FAILED"].contains(&result.as_str()) {
                return Err(
                    "timer finish --result must be DONE, PARTIAL, BLOCKED, or FAILED".to_owned(),
                );
            }
            Ok(Command::Timer(TimerCommand::Finish { run_id, result }))
        }
        "list" => {
            validate_options(args, 0, &["--status", "--limit"], &[], "timer list")?;
            let status = optional_option(args, "--status").unwrap_or_else(|| "all".to_owned());
            if !matches!(status.as_str(), "running" | "finished" | "all") {
                return Err("timer list --status must be running, finished, or all".to_owned());
            }
            let limit = match optional_option(args, "--limit") {
                Some(value) => value
                    .parse::<u32>()
                    .map_err(|_| "timer list --limit requires a non-negative integer".to_owned())?,
                None => 100,
            };
            if limit == 0 {
                return Err("timer list --limit must be greater than zero".to_owned());
            }
            Ok(Command::Timer(TimerCommand::List { status, limit }))
        }
        "get" => {
            validate_options(args, 1, &[], &[], "timer get")?;
            let run_id = args
                .get(1)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or_else(|| "timer get requires a run id".to_owned())?;
            Ok(Command::Timer(TimerCommand::Get { run_id }))
        }
        "recover" => {
            validate_options(args, 1, &[], &[], "timer recover")?;
            let run_id = args
                .get(1)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or_else(|| "timer recover requires a run id".to_owned())?;
            Ok(Command::Timer(TimerCommand::Recover { run_id }))
        }
        value => Err(format!("unknown timer command '{value}'")),
    }
}

fn parse_workflow(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Workflow));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::WorkflowCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "bootstrap" => {
            // The shared `AI Agents` group has been retired. Surface a clear
            // usage error so stale callers do not silently get the new
            // behaviour.
            for rejected in ["--group-name", "--group-role"] {
                if args.iter().any(|value| value == rejected)
                    || args
                        .iter()
                        .any(|value| split_inline(value, rejected).is_some())
                {
                    return Err(format!(
                        "workflow bootstrap {rejected} is no longer supported; \
                         direct memberships are reconciled automatically from the \
                         orchestrator, executor, and reviewer API keys",
                    ));
                }
            }
            validate_options(
                args,
                0,
                &["--repository", "--close-status-id", "--close-status-name"],
                &[],
                "workflow bootstrap",
            )?;
            let close_status_id = optional_option(args, "--close-status-id");
            let close_status_name = optional_option(args, "--close-status-name");
            if close_status_id.is_some() && close_status_name.is_some() {
                return Err(
                    "workflow bootstrap accepts either --close-status-id or --close-status-name"
                        .to_owned(),
                );
            }
            Ok(Command::Workflow(WorkflowCommand::Bootstrap {
                repository: optional_option(args, "--repository"),
                close_status_id,
                close_status_name,
            }))
        }
        value => Err(format!("unknown workflow command '{value}'")),
    }
}

fn parse_auth(args: &[String]) -> Result<Command, String> {
    if args
        .first()
        .is_some_and(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::Auth));
    }
    if args.first().map(String::as_str) != Some("setup") {
        return Err("auth requires the setup subcommand".to_owned());
    }
    let mut read_stdin = false;
    let mut provider = None;
    let mut api_base = None;
    let mut repository = None;
    let mut project_id = None;
    let mut close_status_id = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--stdin" => read_stdin = true,
            "--help" | "-h" => return Ok(Command::Help(HelpTopic::Auth)),
            "--provider" => {
                provider = Some(required_value(args, index, "--provider")?.parse()?);
                index += 1;
            }
            "--api-base" => {
                api_base = Some(required_value(args, index, "--api-base")?);
                index += 1;
            }
            "--repository" => {
                repository = Some(required_value(args, index, "--repository")?);
                index += 1;
            }
            "--project-id" => {
                project_id = Some(required_value(args, index, "--project-id")?);
                index += 1;
            }
            "--close-status-id" => {
                close_status_id = Some(required_value(args, index, "--close-status-id")?);
                index += 1;
            }
            value if value.starts_with("--") => {
                if let Some(parsed) = split_inline(value, "--provider") {
                    provider = Some(parsed.parse()?);
                } else if let Some(parsed) = split_inline(value, "--api-base") {
                    api_base = Some(parsed);
                } else if let Some(parsed) = split_inline(value, "--repository") {
                    repository = Some(parsed);
                } else if let Some(parsed) = split_inline(value, "--project-id") {
                    project_id = Some(parsed);
                } else if let Some(parsed) = split_inline(value, "--close-status-id") {
                    close_status_id = Some(parsed);
                } else {
                    return Err(format!("unknown auth setup option '{value}'"));
                }
            }
            value => return Err(format!("unknown auth setup option '{value}'")),
        }
        index += 1;
    }
    Ok(Command::AuthSetup {
        read_stdin,
        provider,
        api_base,
        repository,
        project_id,
        close_status_id,
    })
}

fn parse_config(args: &[String]) -> Result<Command, String> {
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

fn parse_issue(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Issue));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::IssueCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "get" => {
            require_exact_positionals(args, 2, "issue get")?;
            Ok(Command::Issue(IssueCommand::Get {
                number: positional_number(args, 1, "issue get")?,
            }))
        }
        "search" => parse_issue_search(args),
        "create" => {
            validate_options(
                args,
                0,
                &[
                    "--title",
                    "--body",
                    "--tracker",
                    "--parent-issue",
                    "--fixed-version",
                    "--start-date",
                    "--due-date",
                    "--estimated-hours",
                    "--done-ratio",
                ],
                &[],
                "issue create",
            )?;
            Ok(Command::Issue(IssueCommand::Create {
                title: required_option(args, "--title", "issue create")?,
                body: optional_option(args, "--body").unwrap_or_default(),
                tracker: optional_option(args, "--tracker"),
                planning: planning_options(args),
            }))
        }
        "update-body" => {
            validate_options(
                args,
                1,
                &[
                    "--body",
                    "--tracker",
                    "--parent-issue",
                    "--fixed-version",
                    "--start-date",
                    "--due-date",
                    "--estimated-hours",
                    "--done-ratio",
                ],
                &[],
                "issue update-body",
            )?;
            Ok(Command::Issue(IssueCommand::UpdateBody {
                number: positional_number(args, 1, "issue update-body")?,
                body: required_option(args, "--body", "issue update-body")?,
                tracker: optional_option(args, "--tracker"),
                planning: planning_options(args),
            }))
        }
        "close" => {
            require_exact_positionals(args, 2, "issue close")?;
            Ok(Command::Issue(IssueCommand::Close {
                number: positional_number(args, 1, "issue close")?,
            }))
        }
        "bind" => {
            validate_options(args, 1, &[], &["--replace"], "issue bind")?;
            let issue_id = positional_number(args, 1, "issue bind")?;
            if issue_id == 0 {
                return Err("issue bind requires a positive issue id".to_owned());
            }
            Ok(Command::Issue(IssueCommand::Bind {
                issue_id,
                replace: has_flag(args, "--replace"),
            }))
        }
        "unbind" => {
            require_exact_positionals(args, 1, "issue unbind")?;
            Ok(Command::Issue(IssueCommand::Unbind))
        }
        // Local branch context status; unrelated to the provider-backed
        // top-level `status list` command.
        "status" => {
            require_exact_positionals(args, 1, "issue status")?;
            Ok(Command::Issue(IssueCommand::StatusBranch))
        }
        value => Err(format!("unknown issue command '{value}'")),
    }
}

fn parse_issue_search(args: &[String]) -> Result<Command, String> {
    validate_options(args, 0, &["--query", "-q", "--state"], &[], "issue search")?;
    let query = optional_option(args, "--query").or_else(|| optional_option(args, "-q"));
    let state = optional_option(args, "--state").unwrap_or_else(|| "all".to_owned());
    if !matches!(state.as_str(), "open" | "closed" | "all") {
        return Err("--state must be open, closed, or all".to_owned());
    }
    Ok(Command::Issue(IssueCommand::Search { query, state }))
}

fn parse_comment(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Comment));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::CommentCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "create" => {
            validate_options(
                args,
                1,
                &["--body", "--marker"],
                &["--authorized"],
                "comment create",
            )?;
            Ok(Command::Comment(CommentCommand::Create {
                issue: positional_number(args, 1, "comment create")?,
                body: required_option(args, "--body", "comment create")?,
                marker: required_nonempty_option(args, "--marker", "comment create")?,
                authorized: has_flag(args, "--authorized"),
            }))
        }
        "get" => {
            require_exact_positionals(args, 3, "comment get")?;
            Ok(Command::Comment(CommentCommand::Get {
                issue: positional_number(args, 1, "comment get")?,
                comment: positional_number(args, 2, "comment get")?,
            }))
        }
        "find-marker" => {
            validate_options(args, 1, &["--marker"], &[], "comment find-marker")?;
            Ok(Command::Comment(CommentCommand::FindMarker {
                issue: positional_number(args, 1, "comment find-marker")?,
                marker: required_nonempty_option(args, "--marker", "comment find-marker")?,
            }))
        }
        value => Err(format!("unknown comment command '{value}'")),
    }
}

fn parse_project(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Project));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::ProjectCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 1, "project list")?;
            Ok(Command::Project(ProjectCommand::List))
        }
        "create" => {
            validate_options(
                args,
                0,
                &["--name", "--identifier", "--description"],
                &["--confirm"],
                "project create",
            )?;
            if !has_flag(args, "--confirm") {
                return Err("project create requires --confirm".to_owned());
            }
            Ok(Command::Project(ProjectCommand::Create {
                name: required_nonempty_option(args, "--name", "project create")?,
                identifier: required_nonempty_option(args, "--identifier", "project create")?,
                description: optional_option(args, "--description"),
                confirmed: has_flag(args, "--confirm"),
            }))
        }
        value => Err(format!("unknown project command '{value}'")),
    }
}

fn parse_status(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Status));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::StatusCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 1, "status list")?;
            Ok(Command::Status(StatusCommand::List))
        }
        "next" => {
            require_exact_positionals(args, 2, "status next")?;
            Ok(Command::Status(StatusCommand::Next {
                number: positional_number(args, 1, "status next")?,
            }))
        }
        "set" => {
            validate_options(args, 1, &["--status"], &[], "status set")?;
            Ok(Command::Status(StatusCommand::Set {
                number: positional_number(args, 1, "status set")?,
                status: required_nonempty_option(args, "--status", "status set")?,
            }))
        }
        "advance" => {
            validate_options(args, 1, &["--status"], &[], "status advance")?;
            Ok(Command::Status(StatusCommand::Advance {
                number: positional_number(args, 1, "status advance")?,
                status: required_nonempty_option(args, "--status", "status advance")?,
            }))
        }
        value => Err(format!("unknown status command '{value}'")),
    }
}

fn parse_version(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || name == Some("--help") || name == Some("-h") {
        return Ok(Command::Help(HelpTopic::Version));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::VersionCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "list" => {
            require_exact_positionals(args, 1, "version list")?;
            Ok(Command::VersionCommand(VersionCommand::List))
        }
        value => Err(format!("unknown version command '{value}'")),
    }
}

fn parse_relation(args: &[String]) -> Result<Command, String> {
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

fn parse_hooks(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || matches!(name, Some("--help" | "-h")) {
        return Ok(Command::Help(HelpTopic::Hooks));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(Command::Help(HelpTopic::HooksCommand(
            name.unwrap().to_owned(),
        )));
    }
    match name.unwrap() {
        "install" => {
            require_exact_positionals(args, 1, "hooks install")?;
            Ok(Command::Hooks(HooksCommand::Install))
        }
        "run" => parse_hooks_run(args),
        value => Err(format!("unknown hooks command '{value}'")),
    }
}

/// Parses the hidden internal `hooks run` forms invoked by generated hook
/// scripts: `hooks run prepare-commit-msg <file> [source]` and
/// `hooks run commit-msg <file>`. Exact argument counts are validated here
/// and again at execution time.
fn parse_hooks_run(args: &[String]) -> Result<Command, String> {
    let hook_name = args.get(1).map(String::as_str).ok_or_else(|| {
        "hooks run requires a hook name: prepare-commit-msg or commit-msg".to_owned()
    })?;
    let hook = crate::hooks::HookKind::parse(hook_name)
        .ok_or_else(|| format!("unknown hooks run target '{hook_name}'"))?;
    let rest = &args[2..];
    if rest.iter().any(|value| value.starts_with('-')) {
        return Err("hooks run takes only positional arguments".to_owned());
    }
    // Exact counts: prepare-commit-msg takes a file plus optional source;
    // commit-msg takes exactly one file.
    let valid = match hook {
        crate::hooks::HookKind::PrepareCommitMsg => matches!(rest.len(), 1 | 2),
        crate::hooks::HookKind::CommitMsg => rest.len() == 1,
    };
    if !valid {
        return Err(match hook {
            crate::hooks::HookKind::PrepareCommitMsg => {
                "usage: phasegent hooks run prepare-commit-msg <message-file> [source]".to_owned()
            }
            crate::hooks::HookKind::CommitMsg => {
                "usage: phasegent hooks run commit-msg <message-file>".to_owned()
            }
        });
    }
    Ok(Command::Hooks(HooksCommand::Run {
        hook,
        message_file: rest[0].clone(),
        source: rest.get(1).cloned(),
    }))
}

fn help_topic(
    value: &str,
    subcommand: Option<&str>,
    nested_subcommand: Option<&str>,
) -> Result<HelpTopic, String> {
    match value {
        "issue" => match subcommand {
            None => Ok(HelpTopic::Issue),
            Some(value) if ["get", "search", "create", "update-body", "close"].contains(&value) => {
                Ok(HelpTopic::IssueCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown issue help topic '{value}'")),
        },
        "comment" => match subcommand {
            None => Ok(HelpTopic::Comment),
            Some(value) if ["create", "get", "find-marker"].contains(&value) => {
                Ok(HelpTopic::CommentCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown comment help topic '{value}'")),
        },
        "project" => match subcommand {
            None => Ok(HelpTopic::Project),
            Some(value) if ["list", "create"].contains(&value) => {
                Ok(HelpTopic::ProjectCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown project help topic '{value}'")),
        },
        "status" => match subcommand {
            None => Ok(HelpTopic::Status),
            Some("list") | Some("next") | Some("set") | Some("advance") => {
                Ok(HelpTopic::StatusCommand(subcommand.unwrap().to_owned()))
            }
            Some(value) => Err(format!("unknown status help topic '{value}'")),
        },
        "version" => match subcommand {
            None => Ok(HelpTopic::Version),
            Some("list") => Ok(HelpTopic::VersionCommand("list".to_owned())),
            Some(value) => Err(format!("unknown version help topic '{value}'")),
        },
        "relation" => match subcommand {
            None => Ok(HelpTopic::Relation),
            Some(value) if ["list", "create", "delete"].contains(&value) => {
                Ok(HelpTopic::RelationCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown relation help topic '{value}'")),
        },
        "timer" => match subcommand {
            None => Ok(HelpTopic::Timer),
            Some(value) if ["start", "finish", "list", "get", "recover"].contains(&value) => {
                Ok(HelpTopic::TimerCommand(value.to_owned()))
            }
            Some(value) => Err(format!("unknown timer help topic '{value}'")),
        },
        "workflow" => match subcommand {
            None => Ok(HelpTopic::Workflow),
            Some("bootstrap") => Ok(HelpTopic::WorkflowCommand("bootstrap".to_owned())),
            Some(value) => Err(format!("unknown workflow help topic '{value}'")),
        },
        "auth" => Ok(HelpTopic::Auth),
        "config" => match subcommand {
            None => Ok(HelpTopic::Config),
            Some("show") | Some("import-env") => {
                Ok(HelpTopic::ConfigCommand(subcommand.unwrap().to_owned()))
            }
            Some("provider") => match nested_subcommand {
                None => Ok(HelpTopic::ConfigProvider),
                Some("get") | Some("set") | Some("clear") => Ok(HelpTopic::ConfigProviderCommand(
                    nested_subcommand.unwrap().to_owned(),
                )),
                Some(value) => Err(format!("unknown config provider help topic '{value}'")),
            },
            Some(value) => Err(format!("unknown config help topic '{value}'")),
        },
        "repo" => match subcommand {
            None => Ok(HelpTopic::Repo),
            Some("create") => Ok(HelpTopic::RepoCommand("create".to_owned())),
            Some(value) => Err(format!("unknown repo help topic '{value}'")),
        },
        "ci" => match (subcommand, nested_subcommand) {
            (None, _) => Ok(HelpTopic::Ci),
            (Some("runs"), None) | (Some("inspect"), None) => {
                Ok(HelpTopic::CiCommand(subcommand.unwrap().to_owned()))
            }
            (Some("run"), None) | (Some("job"), None) => {
                Ok(HelpTopic::CiCommand(subcommand.unwrap().to_owned()))
            }
            (Some("run"), Some(value)) if ["get", "jobs"].contains(&value) => {
                Ok(HelpTopic::CiCommand(format!("run {value}")))
            }
            (Some("job"), Some("logs")) => Ok(HelpTopic::CiCommand("job logs".to_owned())),
            (Some(value), _) => Err(format!("unknown ci help topic '{value}'")),
        },
        "hooks" => match subcommand {
            None => Ok(HelpTopic::Hooks),
            Some("install") => Ok(HelpTopic::HooksCommand("install".to_owned())),
            Some(value) => Err(format!("unknown hooks help topic '{value}'")),
        },
        _ => Err(format!("unknown help topic '{value}'")),
    }
}

fn required_value(args: &[String], index: usize, option: &str) -> Result<String, String> {
    // Accept the inline `--option=value` form first so values starting with `-`
    // (Markdown bullets, separator lines, etc.) are not rejected. The two-arg
    // form keeps its strict missing-value detection.
    if let Some(value) = split_inline(&args[index], option) {
        return Ok(value);
    }
    match args.get(index + 1) {
        None => Err(format!("{option} requires a value")),
        Some(value) if value.starts_with('-') => Err(format!(
            "{option} requires a value (use {option}=VALUE when VALUE starts with `-`)"
        )),
        Some(value) => Ok(value.clone()),
    }
}

fn required_option(args: &[String], option: &str, operation: &str) -> Result<String, String> {
    if let Some(value) = optional_option(args, option) {
        return Ok(value);
    }
    // When the option appears but its next token starts with `-`, the parser
    // treats it as missing to keep ambiguous two-arg detection strict. Surface
    // the inline `--option=value` escape hatch so the leading-dash case is
    // discoverable from the error message.
    if args
        .windows(2)
        .any(|values| values[0] == option && values[1].starts_with('-'))
    {
        return Err(format!(
            "{operation} requires a non-empty {option} (use {option}=VALUE when VALUE starts with `-`)"
        ));
    }
    Err(format!("{operation} requires {option}"))
}

pub(crate) fn required_nonempty_option(
    args: &[String],
    option: &str,
    operation: &str,
) -> Result<String, String> {
    let value = required_option(args, option, operation)?;
    if value.trim().is_empty() {
        return Err(format!("{operation} requires a non-empty {option}"));
    }
    Ok(value)
}

pub(crate) fn optional_option(args: &[String], option: &str) -> Option<String> {
    // Inline `--option=value` form bypasses the leading-dash guard so
    // legitimate values like `- Goal` or `---` are not lost.
    if let Some(value) = args.iter().find_map(|arg| split_inline(arg, option)) {
        return Some(value);
    }
    args.windows(2)
        .find(|values| values[0] == option)
        .and_then(|values| (!values[1].starts_with('-')).then(|| values[1].clone()))
}

/// If `arg` has the form `--option=value`, return the value with `option` matching
/// the full long-name prefix (e.g. `--body` does not match `--bodyline`). Used so
/// that recognized value-bearing options can carry values that legitimately begin
/// with `-` without breaking the existing strict missing-value behavior.
fn split_inline(arg: &str, option: &str) -> Option<String> {
    if arg.len() > option.len()
        && arg.starts_with(option)
        && arg.as_bytes().get(option.len()).copied() == Some(b'=')
    {
        Some(arg[option.len() + 1..].to_owned())
    } else {
        None
    }
}

fn positional_number(args: &[String], index: usize, operation: &str) -> Result<u64, String> {
    args.get(index)
        .ok_or_else(|| format!("{operation} requires an issue number"))?
        .parse()
        .map_err(|_| format!("{operation} requires a numeric issue number"))
}

pub(crate) fn require_exact_positionals(
    args: &[String],
    expected: usize,
    operation: &str,
) -> Result<(), String> {
    if args.len() != expected {
        return Err(format!("{operation} has unexpected arguments"));
    }
    Ok(())
}

pub(crate) fn validate_options(
    args: &[String],
    expected_positionals: usize,
    value_options: &[&str],
    flag_options: &[&str],
    operation: &str,
) -> Result<(), String> {
    let mut positionals = 0;
    let mut index = 1;
    while index < args.len() {
        let value = &args[index];
        if value.starts_with('-') {
            if flag_options.contains(&value.as_str()) {
                index += 1;
                continue;
            }
            // Inline `--option=value` form is recognized so leading-dash values are accepted.
            // The actual value is later extracted by `optional_option`/`required_option`;
            // here we only need to confirm the option is recognized and advance by one token.
            if value_options
                .iter()
                .any(|option| split_inline(value, option).is_some())
            {
                index += 1;
                continue;
            }
            if value_options.contains(&value.as_str()) {
                match args.get(index + 1) {
                    None => {
                        return Err(format!("{value} requires a value"));
                    }
                    Some(next) if next.starts_with('-') => {
                        return Err(format!(
                            "{value} requires a value (use {value}=VALUE when VALUE starts with `-`)"
                        ));
                    }
                    Some(_) => {}
                }
                index += 2;
                continue;
            }
            return Err(format!("unknown option '{value}'"));
        }
        positionals += 1;
        if positionals > expected_positionals {
            return Err(format!("{operation} has unexpected arguments"));
        }
        index += 1;
    }
    if positionals != expected_positionals {
        return Err(format!("{operation} has missing arguments"));
    }
    Ok(())
}

pub(crate) fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|value| value == flag)
}

/// Extract every native planning flag from an issue create/update-body
/// invocation. Values stay raw; semantic validation happens at execution
/// time so error messages can reference the exact operation context.
fn planning_options(args: &[String]) -> PlanningOptions {
    PlanningOptions {
        parent_issue: optional_option(args, "--parent-issue"),
        fixed_version: optional_option(args, "--fixed-version"),
        start_date: optional_option(args, "--start-date"),
        due_date: optional_option(args, "--due-date"),
        estimated_hours: optional_option(args, "--estimated-hours"),
        done_ratio: optional_option(args, "--done-ratio"),
    }
}
