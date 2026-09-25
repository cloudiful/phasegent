use crate::policy::Role;
use crate::providers::ProviderKind;

pub use crate::hooks::HooksCommand;
pub use crate::repo_command::RepoCommand;

mod admin;
mod argv;
mod auth;
mod comment;
mod config;
mod global_options;
mod help_topic;
mod hooks;
mod issue;
mod issue_args;
mod local_args;
mod mcp;
mod notify;
mod parse_helpers;
mod plugin;
mod prelude;
mod project;
mod provider_args;
// Phase 1 command registry skeleton (issue 597): top-level parser routing
// consults `registry::top_level`; the remaining descriptors are consumed by
// the help/dispatch wiring in later phases and are intentionally unused here.
#[allow(dead_code)]
mod registry;
mod relation;
mod status;
mod timer;
mod version;
mod workflow;
mod worktree;
mod worktree_args;

pub use argv::parse;
#[cfg(test)]
pub(crate) use argv::parse_with_role_env;
pub use issue_args::{AssigneeOption, BranchOption, IssueCommand, PlanningOptions};
pub use local_args::{McpCommand, McpTransport, NotifyCommand, PluginCommand};
pub(crate) use parse_helpers::{has_flag, optional_option, validate_options};
pub use provider_args::{
    CommentCommand, ProjectCommand, RelationCommand, StatusCommand, TimerCommand, VersionCommand,
    WorkflowCommand,
};
pub use worktree_args::WorktreeCommand;

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
    ConfigShow,
    /// bearer key is never accepted as a direct value; it must be
    /// supplied via `--stdin` or the secure interactive prompt.
    /// Output uses canonical names and never echoes values.
    ConfigSet {
        setting: String,
        value: Option<String>,
        stdin: bool,
    },
    ConfigClear {
        setting: String,
    },
    ConfigProviderGet,
    ConfigProviderSet {
        value: ProviderKind,
    },
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
    Plugin(PluginCommand),
    Relation(RelationCommand),
    /// Orchestrator-owned local phase timer and Redmine Time Entry
    /// projection. The child executor/reviewer roles do not call this CLI.
    Timer(TimerCommand),
    /// Local worktree leases (issue #239). Acquires/releases per-(repo,
    /// issue, session) worktrees backed by the `worktree_leases` table;
    /// reports status and lists existing leases; prunes clean+expired
    /// retained worktrees. Mutating subcommands are
    /// orchestrator-only at execution time.
    Worktree(WorktreeCommand),
    /// Read-only self-check: credential presence (fingerprint, never
    /// values), index backend state, and the masked PostgreSQL URL.
    /// Usable without a role; the approved replacement for schema
    /// dumps and raw setting reads.
    Doctor,
    /// Explicit desktop entry point for the single-binary shell.
    /// `phasegent gui` opens the Tauri window; every other CLI command
    /// never initializes the GUI. Usable without a role because the
    /// shell is an operator-local launcher, not a role-scoped
    /// provider operation.
    Gui,
    /// Manual-only agent notifications via `cloudiful-notifier`.
    /// `notify send` delivers a bounded envelope on the configured
    /// channel; there are no automatic triggers and no post-success
    /// side effects. Requires a role.
    Notify(NotifyCommand),
    /// rmcp MCP server over stdio (default) or streamable HTTP on
    /// `/mcp`. Tools run with the server-side role from `PHASEGENT_ROLE`
    /// and provider flags; clients never supply a role. Requires a role.
    Mcp(McpCommand),
}

#[derive(Debug)]
pub enum HelpTopic {
    Root,
    Admin,
    Doctor,
    Gui,
    Issue,
    Comment,
    Project,
    Status,
    Auth,
    Config,
    ConfigCommand(String),
    ConfigProvider,
    ConfigProviderCommand(String),
    Notify,
    NotifyCommand(String),
    Mcp,
    McpCommand(String),
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
    Plugin,
    PluginCommand(String),
    Relation,
    RelationCommand(String),
    Timer,
    TimerCommand(String),
    Worktree,
    WorktreeCommand(String),
}
