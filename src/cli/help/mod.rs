use crate::command::HelpTopic;
use crate::policy::Role;
use crate::providers::ProviderKind;

pub mod auth;
pub mod comment;
pub mod common;
pub mod config;
pub mod hooks;
pub mod issue;
pub mod project;
pub mod relation;
pub mod root;
pub mod status;
pub mod timer;
pub mod version;
pub mod workflow;

use auth::print_auth_help;
use comment::{print_comment_command_help, print_comment_help};
use common::print_not_supported_help;
use config::{
    print_config_command_help, print_config_help, print_config_provider_command_help,
    print_config_provider_help,
};
use hooks::{print_hooks_command_help, print_hooks_help};
use issue::{print_issue_command_help, print_issue_help};
use project::{print_project_command_help, print_project_help};
use relation::{print_relation_command_help, print_relation_help};
use root::print_root_help;
use status::{print_status_command_help, print_status_help};
use timer::{print_timer_command_help, print_timer_help};
use version::{print_version_command_help, print_version_help};
use workflow::{print_workflow_command_help, print_workflow_help};

pub(crate) fn print_help(role: Option<Role>, provider: Option<ProviderKind>, topic: HelpTopic) {
    match topic {
        HelpTopic::Root => print_root_help(role, provider),
        HelpTopic::Issue => print_issue_help(role),
        HelpTopic::Comment => print_comment_help(role),
        HelpTopic::Project => print_project_help(role),
        HelpTopic::Status => print_status_help(role),
        HelpTopic::Version => print_version_help(role),
        HelpTopic::Workflow => print_workflow_help(role),
        HelpTopic::Auth => print_auth_help(role),
        HelpTopic::Config => print_config_help(role),
        HelpTopic::ConfigCommand(command) => print_config_command_help(role, &command),
        HelpTopic::ConfigProvider => print_config_provider_help(),
        HelpTopic::ConfigProviderCommand(command) => print_config_provider_command_help(&command),
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
        HelpTopic::Ci => {
            if provider == Some(ProviderKind::Redmine) {
                print_not_supported_help("ci")
            } else {
                crate::ci_cli::print_help(role)
            }
        }
        HelpTopic::CiCommand(command) => {
            if provider == Some(ProviderKind::Redmine) {
                print_not_supported_help(&format!("ci {command}"))
            } else {
                crate::ci_cli::print_command_help(role, &command)
            }
        }
        HelpTopic::Hooks => print_hooks_help(),
        HelpTopic::HooksCommand(command) => print_hooks_command_help(&command),
    }
}
