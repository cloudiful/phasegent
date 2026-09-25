use crate::command::{HelpTopic, Unavailable};
use crate::policy::Role;
use crate::providers::ProviderKind;

pub mod admin;
pub mod auth;
pub mod comment;
pub mod common;
pub mod config;
pub mod doctor;
pub mod hooks;
pub mod issue;
pub mod notify;
pub mod plugin;
pub mod project;
pub mod relation;
pub mod root;
pub mod status;
pub mod timer;
pub mod version;
pub mod workflow;
pub mod worktree;

use admin::print_admin_help;
use auth::print_auth_help;
use comment::{print_comment_command_help, print_comment_help};
use common::print_not_supported_help;
use config::{
    print_admin_config_command_help, print_admin_config_help,
    print_admin_config_provider_command_help, print_admin_config_provider_help,
    print_config_command_help, print_config_help, print_config_provider_command_help,
    print_config_provider_help,
};
use doctor::print_doctor_help;
use hooks::{print_hooks_command_help, print_hooks_help};
use issue::{print_issue_command_help, print_issue_help};
use notify::{print_notify_command_help, print_notify_help};
use plugin::{print_plugin_command_help, print_plugin_help};
use project::{print_project_command_help, print_project_help};
use relation::{print_relation_command_help, print_relation_help};
use root::{print_mcp_command_help, print_mcp_help, print_root_help};
use status::{print_status_command_help, print_status_help};
use timer::{print_timer_command_help, print_timer_help};
use version::{print_version_command_help, print_version_help};
use workflow::{print_workflow_command_help, print_workflow_help};
use worktree::{print_worktree_command_help, print_worktree_help};

/// Registry path for a help topic, used by the role-aware help gate. `Root`
/// and unknown detail topics return `None`; group topics map to their group
/// descriptor so a group with no runnable subcommand is denied uniformly.
fn topic_registry_path(topic: &HelpTopic) -> Option<Vec<&str>> {
    Some(match topic {
        HelpTopic::Root => return None,
        HelpTopic::Gui => vec!["gui"],
        HelpTopic::Doctor => vec!["doctor"],
        HelpTopic::Admin => vec!["admin"],
        HelpTopic::Auth => vec!["admin", "auth", "setup"],
        HelpTopic::Config => vec!["config"],
        HelpTopic::ConfigCommand(command) => match command.as_str() {
            "show" => vec!["config", "show"],
            // Defensive: any stray write topic still resolves through the
            // admin gate rather than the open read-only group.
            "set" => vec!["admin", "config", "set"],
            "clear" => vec!["admin", "config", "clear"],
            _ => return None,
        },
        HelpTopic::ConfigProvider => vec!["config", "provider"],
        HelpTopic::ConfigProviderCommand(command) => match command.as_str() {
            "get" => vec!["config", "provider", "get"],
            "set" => vec!["admin", "config", "provider", "set"],
            "clear" => vec!["admin", "config", "provider", "clear"],
            _ => return None,
        },
        HelpTopic::AdminConfig => vec!["admin", "config"],
        HelpTopic::AdminConfigCommand(command) => vec!["admin", "config", command.as_str()],
        HelpTopic::AdminConfigProvider => vec!["admin", "config", "provider"],
        HelpTopic::AdminConfigProviderCommand(command) => {
            vec!["admin", "config", "provider", command.as_str()]
        }
        HelpTopic::Issue => vec!["issue"],
        HelpTopic::IssueCommand(command) => vec!["issue", command.as_str()],
        HelpTopic::Comment => vec!["comment"],
        HelpTopic::CommentCommand(command) => vec!["comment", command.as_str()],
        HelpTopic::Project => vec!["project"],
        HelpTopic::ProjectCommand(command) => vec!["project", command.as_str()],
        HelpTopic::Status => vec!["status"],
        HelpTopic::StatusCommand(command) => vec!["status", command.as_str()],
        HelpTopic::Version => vec!["version"],
        HelpTopic::VersionCommand(command) => vec!["version", command.as_str()],
        HelpTopic::Workflow => vec!["admin", "workflow"],
        HelpTopic::WorkflowCommand(command) => vec!["admin", "workflow", command.as_str()],
        HelpTopic::Repo => vec!["repo"],
        HelpTopic::RepoCommand(command) => vec!["repo", command.as_str()],
        HelpTopic::Hooks => vec!["hooks"],
        HelpTopic::HooksCommand(command) => vec!["hooks", command.as_str()],
        HelpTopic::Plugin => vec!["plugin"],
        HelpTopic::PluginCommand(command) => vec!["plugin", command.as_str()],
        HelpTopic::Relation => vec!["relation"],
        HelpTopic::RelationCommand(command) => vec!["relation", command.as_str()],
        HelpTopic::Timer => vec!["timer"],
        HelpTopic::TimerCommand(command) => vec!["timer", command.as_str()],
        HelpTopic::Notify => vec!["notify"],
        HelpTopic::NotifyCommand(command) => vec!["notify", command.as_str()],
        HelpTopic::Mcp => vec!["mcp"],
        HelpTopic::McpCommand(command) => vec!["mcp", command.as_str()],
        HelpTopic::Worktree => vec!["worktree"],
        HelpTopic::WorktreeCommand(command) => vec!["worktree", command.as_str()],
    })
}

pub(crate) fn print_help(role: Option<Role>, provider: Option<ProviderKind>, topic: HelpTopic) {
    // Registry-driven detail/group gate: a command whose compile-time feature
    // was not compiled and, for a resolved role, a command that role may not
    // run never render their page. A not-compiled page prints the same stable
    // message the execution layer returns; a role-denied page prints the
    // existing denial line, so neither leaks its parameters. No role keeps the
    // compatibility superset pages for compiled commands, and a detail topic
    // the registry does not describe stays with its owning module.
    if let Some(path) = topic_registry_path(&topic)
        && let Some(unavailable) = crate::command::registry_unavailability(role, &path)
    {
        match unavailable {
            Unavailable::NotCompiled(feature) => println!("{}", feature.not_compiled_message()),
            Unavailable::RoleDenied { role, .. } => {
                println!("No command available for {}.", role.as_str());
            }
        }
        return;
    }
    match topic {
        HelpTopic::Root => print_root_help(role, provider),
        HelpTopic::Gui => print_gui_help(),
        HelpTopic::Issue => print_issue_help(role),
        HelpTopic::Comment => print_comment_help(role),
        HelpTopic::Doctor => print_doctor_help(),
        HelpTopic::Project => print_project_help(role),
        HelpTopic::Status => print_status_help(role),
        HelpTopic::Version => print_version_help(role),
        HelpTopic::Admin => print_admin_help(role),
        HelpTopic::Workflow => print_workflow_help(role),
        HelpTopic::Auth => print_auth_help(role),
        HelpTopic::Config => print_config_help(role),
        HelpTopic::ConfigCommand(command) => print_config_command_help(role, &command),
        HelpTopic::ConfigProvider => print_config_provider_help(role),
        HelpTopic::ConfigProviderCommand(command) => {
            print_config_provider_command_help(role, &command)
        }
        HelpTopic::AdminConfig => print_admin_config_help(role),
        HelpTopic::AdminConfigCommand(command) => print_admin_config_command_help(role, &command),
        HelpTopic::AdminConfigProvider => print_admin_config_provider_help(role),
        HelpTopic::AdminConfigProviderCommand(command) => {
            print_admin_config_provider_command_help(role, &command)
        }
        HelpTopic::Repo => {
            if provider == Some(ProviderKind::Redmine) {
                print_not_supported_help("repo")
            } else {
                crate::repo_cli::print_help(role)
            }
        }
        HelpTopic::IssueCommand(command) => print_issue_command_help(role, &command),
        HelpTopic::CommentCommand(command) => print_comment_command_help(role, &command),
        HelpTopic::ProjectCommand(command) => print_project_command_help(role, &command),
        HelpTopic::StatusCommand(command) => print_status_command_help(role, &command),
        HelpTopic::VersionCommand(command) => print_version_command_help(role, &command),
        HelpTopic::WorkflowCommand(command) => print_workflow_command_help(role, &command),
        HelpTopic::Relation => print_relation_help(role),
        HelpTopic::RelationCommand(command) => print_relation_command_help(role, &command),
        HelpTopic::Timer => print_timer_help(role),
        HelpTopic::TimerCommand(command) => print_timer_command_help(role, &command),
        HelpTopic::RepoCommand(command) => {
            if provider == Some(ProviderKind::Redmine) {
                print_not_supported_help(&format!("repo {command}"))
            } else {
                crate::repo_cli::print_command_help(role, &command, provider)
            }
        }
        HelpTopic::Hooks => print_hooks_help(),
        HelpTopic::HooksCommand(command) => print_hooks_command_help(&command),
        HelpTopic::Plugin => print_plugin_help(),
        HelpTopic::PluginCommand(command) => print_plugin_command_help(&command),
        HelpTopic::Notify => print_notify_help(role),
        HelpTopic::NotifyCommand(command) => print_notify_command_help(role, &command),
        HelpTopic::Mcp => print_mcp_help(role),
        HelpTopic::McpCommand(command) => print_mcp_command_help(role, &command),
        HelpTopic::Worktree => print_worktree_help(role),
        HelpTopic::WorktreeCommand(command) => print_worktree_command_help(role, &command),
    }
}

/// Help for the explicit desktop entry. Kept short and task-oriented
/// so root help stays compact; documents the single-binary dispatch
/// and the conservative no-argument desktop heuristic.
fn print_gui_help() {
    println!(
        "Usage: phasegent gui\n\nOpen the desktop GUI in the same binary (Tauri shell).\n\nCLI commands never start the GUI. A bare launch with no arguments shows CLI help in a terminal; an Explorer/Finder-style launch with no console opens the GUI only when a desktop session is detectable, otherwise it also shows CLI help. GUI builds require --features gui; without the feature `phasegent gui` reports a structured gui error."
    );
}
