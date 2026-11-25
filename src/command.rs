use crate::policy::{CallerContext, Role};
use crate::providers::ProviderKind;

pub use crate::hooks::HooksCommand;

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
mod logs;
mod notify;
mod parse_helpers;
mod plugin;
mod prelude;
mod project;
mod provider_args;
mod record;
mod record_args;
mod skill;
// Phase 1 command registry skeleton (issue 597): top-level parser routing
// consults `registry::top_level`; Phase 2 wires the same tree into the
// parser role gate, root help, and the help detail gate; Phase 3 wires the
// compile-time feature boundary into visibility and the not-compiled help
// result.
#[allow(dead_code)]
mod registry;
mod registry_path;
mod relation;
mod status;
mod timer;
mod version;
mod workflow;
mod worktree;
mod worktree_args;

pub(crate) use argv::caller_from_env;
#[allow(unused_imports)] // public parser entry used by the fuzz harness
pub use argv::parse;
#[cfg(test)]
pub(crate) use argv::parse_with_role_env;
pub(crate) use argv::{
    ParseOutcome, is_managed_without_role_denial, parse_outcome, permission_message,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use argv::{parse_outcome_with_caller_env, parse_with_caller_env};
pub(crate) use global_options::prescan_presentation;
pub use issue_args::{AssigneeOption, BranchOption, IssueCommand, PlanningOptions};
pub use local_args::{NotifyCommand, PluginCommand, SkillCommand};
pub(crate) use logs::DEFAULT_LIMIT;
pub use logs::LogsCommand;
pub(crate) use parse_helpers::{has_flag, optional_option};
pub use provider_args::{
    CommentCommand, ProjectCommand, RelationCommand, StatusCommand, TimerCommand, VersionCommand,
    WorkflowCommand,
};
pub use record_args::RecordCommand;
pub(crate) use registry::Unavailable;
pub use worktree_args::WorktreeCommand;

#[derive(Debug)]
pub struct Invocation {
    pub role: Option<Role>,
    /// Invocation policy context derived from the explicit role plus the
    /// managed-session marker. Authorization gates use this, not the raw
    /// role; credential resolution uses its `credential_role` accessor.
    pub caller: CallerContext,
    pub provider: Option<ProviderKind>,
    pub api_base: Option<String>,
    pub repository: Option<String>,
    pub project_id: Option<String>,
    pub close_status_id: Option<String>,
    pub close_status_name: Option<String>,
    /// The presentation options of this run: the global `--output`
    /// selection, the progress switch, and an explicit legacy per-command
    /// format that overrides the global selection for that command.
    pub output: crate::output::context::OutputOptions,
    pub command: Command,
}

impl Invocation {
    /// Central credential routing for provider-dependent operations:
    /// Operator resolves to the existing admin credential, an Agent to its
    /// own role. `None` only for a managed session without a role, which
    /// fails closed before any credential use.
    #[allow(dead_code)]
    pub fn credential_role(&self) -> Option<Role> {
        self.caller.credential_role()
    }
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
        /// Human-only credential storage scope. Never the caller identity.
        target_role: Option<Role>,
    },
    ConfigShow,
    /// bearer key is never accepted as a direct value; it must be
    /// supplied via `--stdin` or the secure interactive prompt.
    /// Output uses canonical names and never echoes values.
    ConfigSet {
        setting: String,
        value: Option<String>,
        stdin: bool,
        /// Human-only setting storage scope. Never the caller identity.
        target_role: Option<Role>,
    },
    ConfigClear {
        setting: String,
        /// Human-only setting storage scope. Never the caller identity.
        target_role: Option<Role>,
    },
    ConfigProviderGet,
    ConfigProviderSet {
        value: ProviderKind,
    },
    ConfigProviderClear,
    /// Human-operator migration of the legacy SQLite/TOML configuration
    /// into the JSONC configuration file. The source database is named
    /// explicitly; the destination uses the configuration resolver.
    ConfigMigrate {
        /// Absolute path of the legacy phasegent database.
        from_sqlite: String,
        /// Absolute path of an explicit legacy TOML overlay.
        from_toml: Option<String>,
    },
    /// Human-operator report and explicit upgrade of a version-1 file.
    ConfigUpgrade,
    Issue(IssueCommand),
    Comment(CommentCommand),
    /// Structured agent records: a CLI-owned versioned envelope over the
    /// existing comment primitives, so an agent publishes metadata plus a
    /// plain note and gets a stable native reference back.
    Record(RecordCommand),
    Project(ProjectCommand),
    Status(StatusCommand),
    /// Redmine project version operations; named to stay distinct from the
    /// pre-existing `Command::Version` (`--version`) variant.
    VersionCommand(VersionCommand),
    Workflow(WorkflowCommand),
    Hooks(HooksCommand),
    Plugin(PluginCommand),
    /// Single bundled skill installation (`phasegent skill install`).
    Skill(SkillCommand),
    Relation(RelationCommand),
    /// Orchestrator-owned local phase timer and Redmine Time Entry
    /// projection. The child executor/reviewer roles do not call this CLI.
    Timer(TimerCommand),
    /// Registered local worktree directories. Acquires or reuses a
    /// per-(repo, issue, session) directory, reports issue associations, lists
    /// every directory for a repository, and prunes eligible clean worktrees.
    /// Mutating subcommands are orchestrator-only at execution time.
    Worktree(WorktreeCommand),
    /// Read-only self-check: credential presence (fingerprint, never
    /// values), index backend state, and the masked PostgreSQL URL.
    /// Usable without a role; the approved replacement for schema
    /// dumps and raw setting reads.
    Doctor,
    /// Manual-only agent notifications via `cloudiful-notifier`.
    /// `notify send` delivers a bounded envelope on the configured
    /// channel; there are no automatic triggers and no post-success
    /// side effects. Requires a role.
    Notify(NotifyCommand),
    /// The bounded, read-only view of the diagnostic log that
    /// `worktree prune` leaves by default. It prepares no configuration,
    /// resolves no provider, uses no credential, and opens no registry.
    Logs(LogsCommand),
}

#[derive(Debug)]
pub enum HelpTopic {
    Root,
    Admin,
    Doctor,
    Issue,
    Comment,
    Project,
    Status,
    Auth,
    Config,
    ConfigCommand(String),
    ConfigProvider,
    ConfigProviderCommand(String),
    /// Human-operator write surface reached only through `admin config`; a
    /// separate topic so AI roles get the stable denial instead of the
    /// read-only top-level page.
    AdminConfig,
    AdminConfigCommand(String),
    AdminConfigProvider,
    AdminConfigProviderCommand(String),
    Notify,
    NotifyCommand(String),
    IssueCommand(String),
    CommentCommand(String),
    Record,
    RecordCommand(String),
    ProjectCommand(String),
    StatusCommand(String),
    Version,
    VersionCommand(String),
    Workflow,
    WorkflowCommand(String),
    Hooks,
    HooksCommand(String),
    Plugin,
    PluginCommand(String),
    Skill,
    SkillCommand(String),
    Relation,
    RelationCommand(String),
    Timer,
    TimerCommand(String),
    Worktree,
    WorktreeCommand(String),
    Logs,
}

pub(crate) fn prepare_at_admission(invocation: &Invocation) -> bool {
    if invocation.caller.is_managed_without_role() {
        return false;
    }
    match &invocation.command {
        Command::Doctor | Command::ConfigShow | Command::ConfigProviderGet => true,
        Command::Issue(IssueCommand::Branches { .. } | IssueCommand::StatusBranch) => false,
        Command::Issue(IssueCommand::UploadAttachment { .. }) => false,
        Command::Issue(IssueCommand::Bind { .. } | IssueCommand::Unbind { .. }) => {
            invocation.provider != Some(ProviderKind::Local)
        }
        Command::Timer(TimerCommand::List { .. } | TimerCommand::Get { .. }) => false,
        Command::Timer(TimerCommand::Finish { .. } | TimerCommand::Recover { .. }) => true,
        Command::Issue(_) => true,
        Command::Comment(CommentCommand::Create { .. })
        | Command::Record(RecordCommand::Create { .. }) => false,
        Command::Comment(_)
        | Command::Record(_)
        | Command::Project(_)
        | Command::Status(_)
        | Command::VersionCommand(_)
        | Command::Relation(_)
        | Command::Timer(_) => invocation.provider != Some(ProviderKind::Local),
        Command::Notify(_) => true,
        _ => false,
    }
}

/// Whether `role` may run the command at a registry `path`. Shared by the
/// parser role gate and the help surface so visibility and acceptance can
/// never drift apart. A node whose compile-time feature is absent is denied.
pub(crate) fn registry_allows_role(role: Role, path: &[&str]) -> bool {
    registry::allows_role(role, path)
}

/// Whether `caller` may run the command at a registry `path`. Operator
/// authorizes every registered path except the agent-authored
/// `record create`; an Agent delegates to its role; a managed session
/// without a role authorizes nothing here (utility passthrough is decided
/// by the argv allowlist, not the registry).
#[allow(dead_code)]
pub(crate) fn registry_allows_caller(caller: CallerContext, path: &[&str]) -> bool {
    registry::allows_caller(caller, path)
}

/// The stable reason a registry `path` is unavailable for `role`, or `None`
/// when it is available or unknown. Used by the help gate so a feature that
/// was not compiled and a role denial stay distinct.
pub(crate) fn registry_unavailability(role: Option<Role>, path: &[&str]) -> Option<Unavailable> {
    registry::unavailability(role, path)
}

/// The stable permission `operation` for a role-denied registry path, or
/// `None` when the path is unknown, allowed, or not compiled.
pub(crate) fn registry_denied_operation(role: Role, path: &[&str]) -> Option<&'static str> {
    registry::denied_operation(role, path)
}

/// The stable permission `operation` for a caller-denied registry path, or
/// `None` when the path is unknown or allowed.
#[allow(dead_code)]
pub(crate) fn registry_denied_operation_for_caller(
    caller: CallerContext,
    path: &[&str],
) -> Option<&'static str> {
    registry::denied_operation_for_caller(caller, path)
}

/// Whether a top-level command is visible for the resolved role and provider.
/// Used by role-aware root help; an uncompiled feature is hidden from every
/// view, including the role-less superset.
pub(crate) fn top_level_visible(
    name: &str,
    role: Option<Role>,
    provider: Option<ProviderKind>,
) -> bool {
    registry::top_level(name)
        .is_some_and(|spec| spec.provider_scope.visible(provider) && spec.visible_for(role))
}

/// The root-help row summary for a registered top-level command.
pub(crate) fn top_level_summary(name: &str) -> Option<&'static str> {
    registry::top_level(name).map(|spec| spec.summary)
}

/// Every registered top-level command name, in declaration order. Used by the
/// root-help completeness test.
#[cfg(test)]
pub(crate) fn top_level_names() -> Vec<&'static str> {
    registry::top_level_names()
}

/// Registry path for one parsed command, or `None` for `Help`/`Version`.
pub(crate) fn command_registry_path(command: &Command) -> Option<Vec<&'static str>> {
    registry_path::command_path(command)
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    fn admission(caller: CallerContext, command: Command, provider: Option<ProviderKind>) -> bool {
        prepare_at_admission(&Invocation {
            role: caller.role(),
            caller,
            provider,
            api_base: None,
            repository: None,
            project_id: None,
            close_status_id: None,
            close_status_name: None,
            output: Default::default(),
            command,
        })
    }
    #[test]
    fn admitted_config_reads_and_exclusions() {
        assert!(admission(
            CallerContext::Agent(Role::Executor),
            Command::ConfigShow,
            None
        ));
        assert!(admission(CallerContext::Operator, Command::Doctor, None));
        assert!(!admission(
            CallerContext::ManagedWithoutRole,
            Command::ConfigShow,
            None
        ));
        assert!(!admission(CallerContext::Operator, Command::Version, None));
        assert!(!admission(
            CallerContext::Operator,
            Command::Timer(TimerCommand::Get {
                run_id: "run".into()
            }),
            None
        ));
        assert!(admission(
            CallerContext::Operator,
            Command::Timer(TimerCommand::Recover {
                run_id: "run".into()
            }),
            Some(ProviderKind::Local)
        ));
        assert!(!admission(
            CallerContext::Operator,
            Command::ConfigUpgrade,
            None
        ));
        assert!(!admission(
            CallerContext::Operator,
            Command::Issue(IssueCommand::StatusBranch),
            None
        ));
        assert!(admission(
            CallerContext::Operator,
            Command::Issue(IssueCommand::Get { number: 1 }),
            Some(ProviderKind::Local)
        ));
        assert!(admission(
            CallerContext::Agent(Role::Orchestrator),
            Command::Issue(IssueCommand::Search {
                query: None,
                state: "open".into(),
                page: 1,
                limit: 20,
                all: false,
                include_body: false
            }),
            Some(ProviderKind::Local)
        ));
        assert!(!admission(
            CallerContext::ManagedWithoutRole,
            Command::Issue(IssueCommand::Get { number: 1 }),
            Some(ProviderKind::Local)
        ));
        assert!(!admission(
            CallerContext::Operator,
            Command::Issue(IssueCommand::Branches {
                number: 1,
                page: None
            }),
            Some(ProviderKind::Local)
        ));
        assert!(!admission(
            CallerContext::Operator,
            Command::Issue(IssueCommand::Bind { issue_ids: vec![1] }),
            Some(ProviderKind::Local)
        ));
        assert!(admission(
            CallerContext::Agent(Role::Executor),
            Command::Issue(IssueCommand::Get { number: 1 }),
            None
        ));
    }
}
