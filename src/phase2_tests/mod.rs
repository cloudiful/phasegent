use crate::auth;
use crate::command;
use crate::infra::storage::Storage;
use crate::policy::{Capability, Role};
use crate::providers::ProviderKind;
use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
use crate::providers::redmine::model::{
    TransitionVerdict, canonical_allowed_next, canonical_status_name, evaluate_transition,
};
use crate::remote;
use crate::worktree::WorktreeRunner;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

mod close_cli;
mod close_cli_cleanup;
mod command_inline_options;
mod command_options;
mod hierarchy_cli;
mod hierarchy_lifecycle;
mod issue_commands;
mod issue_search;
mod policy;
mod provider_config;
mod provider_precedence;
mod remote_resolution;
mod status_policy;
mod support;
mod sync_cli;
mod sync_cli_reports;
mod timer_cli;
mod timer_recovery;
mod workflow;
mod worktree_taxi;

/// Build owned argv for the focused hierarchy CLI tests, which share the
/// same slice-to-`Vec<String>` construction in every case.
pub(crate) fn owned_args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}
