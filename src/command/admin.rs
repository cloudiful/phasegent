//! Human-operator provisioning group: `phasegent admin ...`.
//!
//! `admin` collects every provisioning/write entry point that must never be
//! invoked by an AI role: `auth setup` (credential writes),
//! `config set/clear` and `config provider set/clear` (setting writes), and
//! `workflow bootstrap` (admin-key provisioning). Agent permission rules deny
//! the single `admin` token instead of regexing individual subcommands, and
//! the skill bans the group for orchestrator/executor/reviewer/tester.
//!
//! The group reuses the existing `Command` variants so execution, role
//! gating, and the capability matrix are untouched — only the argv routing
//! changes. Read-only views (`config show`, `config provider get`) stay
//! top-level and are rejected here with a pointer to the top-level form.

use super::config;
use super::{Command, HelpTopic};

pub(crate) const ADMIN_ONLY: &str =
    "human-operator only; AI roles must never invoke the admin group";

/// Error for a provisioning command invoked outside the admin group.
pub(crate) fn moved_error(old: &str, new: &str) -> String {
    format!("`{old}` has moved to `{new}` ({ADMIN_ONLY})")
}

pub(crate) fn parse_admin(args: &[String]) -> Result<Command, String> {
    let head = args.first().map(String::as_str);
    match head {
        None | Some("--help") | Some("-h") => Ok(Command::Help(HelpTopic::Admin)),
        Some("auth") => super::auth::parse_auth(&args[1..]),
        Some("config") => config::parse_config_admin(&args[1..]),
        Some("workflow") => super::workflow::parse_workflow(&args[1..]),
        Some(other) => Err(format!(
            "unknown admin command '{other}' (expected auth, config, or workflow)"
        )),
    }
}
