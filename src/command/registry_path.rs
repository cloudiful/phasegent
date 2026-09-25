//! Typed-command to registry-path mapping (issue 597 Phase 2).
//!
//! The parser resolves a [`Command`] through the shared registry so role
//! gates are enforced once, before any provider or network access. The match
//! is exhaustive over every runtime command variant, so a command the parser
//! can produce always resolves to a descriptor path; `Help` and the
//! `--version` toggle carry no command surface and map to `None`.

use super::{
    Command, CommentCommand, IssueCommand, ProjectCommand, RelationCommand, StatusCommand,
    TimerCommand, VersionCommand, WorkflowCommand, WorktreeCommand,
};
use super::{HooksCommand, McpCommand, NotifyCommand, PluginCommand, RepoCommand};

/// Registry path for one parsed command, or `None` for the role-agnostic
/// `Help`/`Version` toggles.
pub(crate) fn command_path(command: &Command) -> Option<Vec<&'static str>> {
    let path: &[&'static str] = match command {
        Command::Help(_) | Command::Version => return None,
        Command::Gui => &["gui"],
        Command::Doctor => &["doctor"],
        Command::AuthSetup { .. } => &["admin", "auth", "setup"],
        Command::ConfigShow => &["config", "show"],
        Command::ConfigSet { .. } => &["admin", "config", "set"],
        Command::ConfigClear { .. } => &["admin", "config", "clear"],
        Command::ConfigProviderGet => &["config", "provider", "get"],
        Command::ConfigProviderSet { .. } => &["admin", "config", "provider", "set"],
        Command::ConfigProviderClear => &["admin", "config", "provider", "clear"],
        Command::Issue(command) => &["issue", issue_name(command)],
        Command::Comment(command) => &["comment", comment_name(command)],
        Command::Project(command) => &["project", project_name(command)],
        Command::Status(command) => &["status", status_name(command)],
        Command::VersionCommand(VersionCommand::List) => &["version", "list"],
        Command::Workflow(WorkflowCommand::Bootstrap { .. }) => &["admin", "workflow", "bootstrap"],
        Command::Repo(RepoCommand::Create { .. }) => &["repo", "create"],
        Command::Hooks(HooksCommand::Install) => &["hooks", "install"],
        Command::Hooks(HooksCommand::Run { .. }) => &["hooks", "run"],
        Command::Plugin(PluginCommand::Install { .. }) => &["plugin", "install"],
        Command::Plugin(PluginCommand::Status) => &["plugin", "status"],
        Command::Plugin(PluginCommand::Uninstall { .. }) => &["plugin", "uninstall"],
        Command::Relation(command) => &["relation", relation_name(command)],
        Command::Timer(command) => &["timer", timer_name(command)],
        Command::Worktree(command) => &["worktree", worktree_name(command)],
        Command::Notify(NotifyCommand::Send { .. }) => &["notify", "send"],
        Command::Mcp(McpCommand::Serve { .. }) => &["mcp", "serve"],
    };
    Some(path.to_vec())
}

fn issue_name(command: &IssueCommand) -> &'static str {
    match command {
        IssueCommand::Get { .. } | IssueCommand::GetBatch { .. } => "get",
        IssueCommand::Search { .. } => "search",
        IssueCommand::Create { .. } => "create",
        IssueCommand::Update { .. } => "update",
        IssueCommand::Close { .. } => "close",
        IssueCommand::UploadAttachment { .. } => "upload-attachment",
        IssueCommand::Sync { .. } => "sync",
        IssueCommand::Bind { .. } => "bind",
        IssueCommand::Unbind => "unbind",
        IssueCommand::StatusBranch => "status",
    }
}

fn comment_name(command: &CommentCommand) -> &'static str {
    match command {
        CommentCommand::Create { .. } => "create",
        CommentCommand::Get { .. } => "get",
        CommentCommand::List { .. } => "list",
        CommentCommand::FindMarker { .. } => "find-marker",
    }
}

fn project_name(command: &ProjectCommand) -> &'static str {
    match command {
        ProjectCommand::List => "list",
        ProjectCommand::Create { .. } => "create",
    }
}

fn status_name(command: &StatusCommand) -> &'static str {
    match command {
        StatusCommand::List => "list",
        StatusCommand::Next { .. } => "next",
        StatusCommand::Set { .. } => "set",
        StatusCommand::Advance { .. } => "advance",
    }
}

fn relation_name(command: &RelationCommand) -> &'static str {
    match command {
        RelationCommand::List { .. } => "list",
        RelationCommand::Create { .. } => "create",
        RelationCommand::Delete { .. } => "delete",
    }
}

fn timer_name(command: &TimerCommand) -> &'static str {
    match command {
        TimerCommand::Start { .. } => "start",
        TimerCommand::Finish { .. } => "finish",
        TimerCommand::List { .. } => "list",
        TimerCommand::Get { .. } => "get",
        TimerCommand::Recover { .. } => "recover",
    }
}

fn worktree_name(command: &WorktreeCommand) -> &'static str {
    match command {
        WorktreeCommand::Acquire { .. } => "acquire",
        WorktreeCommand::Release { .. } => "release",
        WorktreeCommand::Status { .. } => "status",
        WorktreeCommand::List { .. } => "list",
        WorktreeCommand::Probe { .. } => "probe",
        WorktreeCommand::Prune { .. } => "prune",
        WorktreeCommand::Heartbeat { .. } => "heartbeat",
    }
}
