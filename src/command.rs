use crate::policy::Role;
use crate::providers::ProviderKind;
#[allow(unused_imports)]
use crate::providers::api::ForgejoError;
use crate::providers::redmine::model::RedmineRelationType;

pub use crate::hooks::HooksCommand;
pub use crate::repo_command::RepoCommand;

mod auth;
mod comment;
mod config;
mod help_topic;
mod hooks;
mod issue;
mod parse_helpers;
mod project;
mod relation;
mod status;
mod timer;
mod version;
mod workflow;

pub(crate) use parse_helpers::{
    has_flag, optional_option, require_exact_positionals, required_nonempty_option,
    validate_options,
};

use parse_helpers::{required_value, split_inline};

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
        close_status_id: Option<String>,
    },
    /// `config show` — redacted snapshot of the local SQLite
    /// database. The role filter (when supplied) comes from
    /// `Invocation::role`; the command is intentionally usable
    /// without `--role` so an operator can inspect the global view
    /// of configuration state.
    ConfigShow,
    /// `config set <SETTING> [VALUE|--stdin]` — persist a single
    /// setting. Global settings (mirror key, repository URL,
    /// default provider) are machine-wide and usable without
    /// `--role`; role-scoped settings require `--role`. The mirror
    /// bearer key is never accepted as a direct value; it must be
    /// supplied via `--stdin` or the secure interactive prompt.
    /// Output uses canonical names and never echoes values.
    ConfigSet {
        setting: String,
        value: Option<String>,
        stdin: bool,
    },
    /// `config clear <SETTING>` — remove a persisted setting.
    /// Global settings are machine-wide; role-scoped settings
    /// require `--role`.
    ConfigClear {
        setting: String,
    },
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
    /// Redmine-only orchestrator attachment upload. Validates the local
    /// file (exists, regular, non-empty, bounded 25 MiB, valid filename)
    /// then performs the raw `POST /uploads.json?filename=...` plus
    /// `PUT /issues/<id>.json` with `uploads` protocol.
    UploadAttachment {
        number: u64,
        path: String,
        description: Option<String>,
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
        "auth" => auth::parse_auth(rest)?,
        "config" => config::parse_config(rest)?,
        "issue" => issue::parse_issue(rest)?,
        "comment" => comment::parse_comment(rest)?,
        "project" => project::parse_project(rest)?,
        "status" => status::parse_status(rest)?,
        "version" => version::parse_version(rest)?,
        "relation" => relation::parse_relation(rest)?,
        "timer" => timer::parse_timer(rest)?,
        "workflow" => workflow::parse_workflow(rest)?,
        "repo" => crate::repo_command::parse(rest)?,
        "hooks" => hooks::parse_hooks(rest)?,
        value => return Err(format!("unknown command '{value}'")),
    };
    // Local branch context and hooks never touch provider credentials. The
    // internal `hooks run` forms are also invoked by generated Git scripts
    // without a role. `config set/clear` is allowed without --role when
    // the target is a global setting; role-scoped settings still require it.
    let no_role_allowed = match &command {
        Command::Help(_)
        | Command::ConfigShow
        | Command::ConfigProviderGet
        | Command::ConfigProviderSet { .. }
        | Command::ConfigProviderClear
        | Command::Hooks(_)
        | Command::Issue(
            IssueCommand::Bind { .. } | IssueCommand::Unbind | IssueCommand::StatusBranch,
        ) => true,
        Command::ConfigSet { setting, .. } => crate::config_write::is_global_setting(setting),
        Command::ConfigClear { setting } => crate::config_write::is_global_setting(setting),
        _ => false,
    };
    if role.is_none() && !no_role_allowed {
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
