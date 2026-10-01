//! Read-only explorer delegation contract (issue 685 P2, AC 3/4).
//!
//! The explorer surface is a *server-side* delegation: an OpenCode session
//! asks phasegent to run one read-only recon turn through the MCode ACP
//! adapter, and the model never chooses where it runs. This module owns that
//! contract in one place so the MCP handlers never restate it:
//!
//! * which roles may delegate at all ([`DELEGATION_ROLES`]),
//! * what each operation is called and which roles own it
//!   ([`Operation`], [`OPERATION_*`]),
//! * the bounds every caller-supplied value obeys, and
//! * the one place a worktree is resolved: [`BoundTarget::resolve`] takes the
//!   host-bound session and the issue *selector* and requires exactly one
//!   active lease for that pair.
//!
//! Two properties are load-bearing. The issue number is a selector, never an
//! authorization: a caller cannot reach another session's worktree by naming
//! an issue, because the lookup is session-bound and fails closed on both a
//! missing and an ambiguous result. And the resolved worktree never leaves
//! this boundary — it is a server-side path handed to the ACP spawn
//! configuration, never an argument, a result field, or a log line.

use std::path::PathBuf;

use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::worktree::{self, LeaseBindingError};

/// Roles allowed to delegate an explorer run. The orchestrator, executor, and
/// reviewer all delegate recon; `admin` is human-operator only and `tester`
/// runs the allowlisted test commands itself, so neither delegates.
pub const DELEGATION_ROLES: &[Role] = &[Role::Orchestrator, Role::Executor, Role::Reviewer];

/// The five operations the explorer surface exposes, and nothing else: start a
/// run, read its state, wait for it, cancel it, and resume it. There is no
/// run-list, no prompt replay, no run deletion, and no way to name a worktree.
///
/// The enum exists so the operation vocabulary has one definition: the served
/// tool names are `explorer_<action>`, and the surface tests assert each
/// descriptor's name is exactly its operation behind that prefix. It is read by
/// those tests rather than by the handlers, which name their own descriptor.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Start,
    Status,
    Wait,
    Cancel,
    Resume,
}

impl Operation {
    #[cfg_attr(not(test), allow(dead_code))]
    pub const ALL: [Operation; 5] = [
        Operation::Start,
        Operation::Status,
        Operation::Wait,
        Operation::Cancel,
        Operation::Resume,
    ];

    /// The MCP tool name. The host bridge derives the tool id OpenCode exposes
    /// as `phasegent_<action>`, so the action half must stay the same string.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Status => "status",
            Self::Wait => "wait",
            Self::Cancel => "cancel",
            Self::Resume => "resume",
        }
    }
}

/// The permission operation every explorer tool is gated on.
pub const EXPLORER_OPERATION: &str = "explorer delegation";

/// Argument name the host bridge fills in. The model never supplies it: the
/// bridge overwrites the field with its own session id before `tools/call`, and
/// every explorer params struct declares it as required. The surface tests pin
/// the wire spelling so a rename cannot silently unbind the host bridge.
#[allow(dead_code)]
pub const HOST_SESSION_FIELD: &str = "session";

/// Upper bound on a host-bound session id, mirroring the worktree lease's own
/// session bound so one identity can never be spelled two ways.
pub const MAX_SESSION_CHARS: usize = 128;

/// Upper bound on a caller-supplied prompt. Matches the ACP transcript cap, so
/// an over-long prompt is refused at the boundary instead of being truncated
/// into the persisted run row.
pub const MAX_PROMPT_CHARS: usize = crate::mcp::agent::MAX_TRANSCRIPT_CHARS;

/// Default `explorer_wait` budget when the caller names none.
pub const DEFAULT_WAIT_SECS: u64 = 120;

/// Hard ceiling on a caller-supplied wait budget, so one wait call can never
/// pin an MCP connection for the whole prompt budget.
pub const MAX_WAIT_SECS: u64 = 600;

/// Default per-turn prompt budget when the caller names none.
pub const DEFAULT_TURN_SECS: u64 = crate::mcp::agent::DEFAULT_PROMPT_TIMEOUT_SECS;

/// Hard ceiling on a caller-supplied per-turn budget.
pub const MAX_TURN_SECS: u64 = crate::mcp::agent::MAX_PROMPT_TIMEOUT_SECS;

/// Validate a host-bound session id: trimmed, non-empty, bounded, and free of
/// control characters. A blank or over-long identity is refused rather than
/// normalised, so two distinct sessions cannot collapse into one lease key.
pub fn validate_session(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("explorer calls require a host-bound session id".to_owned());
    }
    if trimmed.chars().count() > MAX_SESSION_CHARS {
        return Err(format!(
            "the host-bound session id must be at most {MAX_SESSION_CHARS} characters"
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("the host-bound session id must not contain control characters".to_owned());
    }
    Ok(trimmed.to_owned())
}

/// Validate a prompt and an optional per-turn budget.
pub fn validate_prompt(prompt: &str, timeout_secs: Option<u64>) -> Result<(String, u64), String> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Err("explorer prompt must not be empty".to_owned());
    }
    if trimmed.chars().count() > MAX_PROMPT_CHARS {
        return Err(format!(
            "explorer prompt must be at most {MAX_PROMPT_CHARS} characters"
        ));
    }
    let budget = timeout_secs
        .unwrap_or(DEFAULT_TURN_SECS)
        .clamp(1, MAX_TURN_SECS);
    Ok((trimmed.to_owned(), budget))
}

/// Validate a bounded wait budget, clamping rather than refusing so a caller
/// can ask for "as long as allowed" with one value.
pub fn resolve_wait_budget(timeout_secs: Option<u64>) -> u64 {
    timeout_secs
        .unwrap_or(DEFAULT_WAIT_SECS)
        .clamp(1, MAX_WAIT_SECS)
}

/// The worktree a delegated run executes in, plus the issue that selected it.
///
/// Constructed only by [`BoundTarget::resolve`], so a `BoundTarget` existing at
/// all already means "one active lease for this issue and this session".
/// Deliberately not `Serialize`: the worktree path is server-side data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundTarget {
    pub issue: u64,
    pub worktree: PathBuf,
}

impl BoundTarget {
    /// Resolve the one worktree this session may delegate into.
    ///
    /// `issue` is only a selector. The lease lookup is bound to `session`, so
    /// naming an issue whose lease belongs to another session resolves to
    /// `Missing`, and a session holding two active leases for one issue resolves
    /// to `Ambiguous` rather than picking the newest. Both variants are errors:
    /// there is no fallback to the server's own cwd, to the parent session, or
    /// to a fresh directory.
    pub fn resolve(
        storage: &Storage,
        issue: u64,
        session: &str,
    ) -> Result<BoundTarget, ExplorerBindingError> {
        if issue == 0 {
            return Err(ExplorerBindingError::Argument(
                "the explorer issue selector must be greater than zero".to_owned(),
            ));
        }
        let session = validate_session(session).map_err(ExplorerBindingError::Argument)?;
        worktree::ensure_schema(storage).map_err(ExplorerBindingError::Storage)?;
        let lease = worktree::resolve_active_lease_for_session(storage, issue, &session)
            .map_err(|(reason, _)| ExplorerBindingError::Lease(reason, issue))?;
        Ok(BoundTarget {
            issue,
            worktree: PathBuf::from(lease.worktree_path),
        })
    }
}

/// Why a delegation could not be bound to a worktree. Every variant is a
/// refusal; none of them names the session or the resolved path.
#[derive(Debug)]
pub enum ExplorerBindingError {
    /// A caller-supplied value was unusable.
    Argument(String),
    /// Storage could not answer the lease question.
    Storage(String),
    /// The session-bound lease lookup failed closed.
    Lease(LeaseBindingError, u64),
}

impl ExplorerBindingError {
    /// Bounded, path-free message for a client.
    pub fn message(&self) -> String {
        match self {
            Self::Argument(message) => crate::mcp::agent::error::sanitize(message),
            Self::Storage(message) => crate::mcp::agent::error::sanitize(&format!(
                "explorer lease lookup failed: {message}"
            )),
            Self::Lease(reason, issue) => reason.message(*issue),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_roles_are_orchestrator_executor_reviewer() {
        assert_eq!(
            DELEGATION_ROLES,
            &[Role::Orchestrator, Role::Executor, Role::Reviewer]
        );
        for role in [Role::Admin, Role::Tester] {
            assert!(
                !DELEGATION_ROLES.contains(&role),
                "{role} must not delegate explorer runs"
            );
        }
    }

    #[test]
    fn every_operation_names_a_distinct_tool_action() {
        let mut actions: Vec<&str> = Operation::ALL.iter().map(|op| op.action()).collect();
        actions.sort_unstable();
        actions.dedup();
        assert_eq!(actions.len(), Operation::ALL.len());
        assert_eq!(Operation::ALL.len(), 5);
    }

    #[test]
    fn session_validation_refuses_blank_overlong_and_control_values() {
        assert_eq!(validate_session("  ses_1 ").unwrap(), "ses_1");
        assert!(validate_session("   ").is_err());
        assert!(validate_session("").is_err());
        assert!(validate_session(&"s".repeat(MAX_SESSION_CHARS + 1)).is_err());
        assert!(validate_session("ses\n1").is_err());
    }

    #[test]
    fn prompt_validation_bounds_text_and_clamps_the_turn_budget() {
        let (prompt, budget) = validate_prompt("  map the call flow  ", None).unwrap();
        assert_eq!(prompt, "map the call flow");
        assert_eq!(budget, DEFAULT_TURN_SECS);
        assert!(validate_prompt("   ", None).is_err());
        assert!(validate_prompt(&"x".repeat(MAX_PROMPT_CHARS + 1), None).is_err());
        let (_, budget) = validate_prompt("go", Some(0)).unwrap();
        assert_eq!(budget, 1);
        let (_, budget) = validate_prompt("go", Some(u64::MAX)).unwrap();
        assert_eq!(budget, MAX_TURN_SECS);
    }

    #[test]
    fn wait_budget_is_always_bounded() {
        assert_eq!(resolve_wait_budget(None), DEFAULT_WAIT_SECS);
        assert_eq!(resolve_wait_budget(Some(0)), 1);
        assert_eq!(resolve_wait_budget(Some(u64::MAX)), MAX_WAIT_SECS);
    }

    #[test]
    fn binding_error_messages_name_no_path_or_session() {
        for reason in [
            LeaseBindingError::Missing,
            LeaseBindingError::Ambiguous,
            LeaseBindingError::Unusable,
        ] {
            let message = reason.message(685);
            assert!(message.contains("685"), "{message}");
            assert!(!message.contains('/'), "{message}");
            assert!(!message.contains("ses_"), "{message}");
        }
    }
}
