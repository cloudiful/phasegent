//! Focused unit tests for the worktree directory CLI surface.
//!
//! Coverage mirrors the directory contract: parsing of the remaining
//! subcommands and the role gates.

pub(crate) use crate::cli::worktree::execute_worktree;
use crate::command::{Command, WorktreeCommand};
use crate::policy::Role;

mod help_routing;
mod parse_acquire;
mod parse_probe;
mod parse_prune;
mod parse_status_list;
mod role_gate;
mod support;
