//! Generic research delegation contract (issue 692 P1).
//!
//! The research surface is a *server-side* delegation: an OpenCode session
//! asks phasegent to run one read-only research turn through the MCode ACP
//! adapter, and the model never chooses where it runs. This module owns that
//! contract in one place so the MCP handlers never restate it:
//!
//! * which roles may delegate at all ([`DELEGATION_ROLES`]),
//! * what each operation is called and which roles own it ([`Operation`]),
//! * the bounds every caller-supplied value obeys, and
//! * the fixed server-owned read-only instruction the ACP adapter prepends
//!   to every caller prompt (defined next to the prompt type in
//!   [`crate::mcp::agent`]).
//!
//! There is no issue selector and no worktree lookup. Each run executes in a
//! private server-created scratch directory; the only inputs a caller supplies
//! are the research prompt and a bounded per-turn budget. Host-session
//! ownership is transport metadata: the OpenCode bridge injects it and the
//! server uses it solely to isolate run controls, never as an issue, worktree,
//! checkout, or repository context.

use crate::policy::Role;

/// Roles allowed to delegate a research run. The orchestrator, executor, and
/// reviewer all delegate research; `admin` is human-operator only and `tester`
/// runs the allowlisted test commands itself, so neither delegates.
pub const DELEGATION_ROLES: &[Role] = &[Role::Orchestrator, Role::Executor, Role::Reviewer];

/// The five operations the research surface exposes, and nothing else: start a
/// run, read its state, wait for it, cancel it, and resume it. There is no
/// run-list, no prompt replay, no run deletion, and no way to name a location.
///
/// The enum exists so the operation vocabulary has one definition: the served
/// tool names are `research_<action>`, and the surface tests assert each
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

/// The permission operation every research tool is gated on.
pub const RESEARCH_OPERATION: &str = "research delegation";

/// Argument name the host bridge fills in. The model never supplies it: the
/// bridge overwrites the field with its own session id before `tools/call`, and
/// every research params struct declares it as required. The surface tests pin
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

/// Default `research_wait` budget when the caller names none.
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
/// normalised, so two distinct sessions cannot collapse into one owner key.
pub fn validate_session(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("research calls require a host-bound session id".to_owned());
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
        return Err("research prompt must not be empty".to_owned());
    }
    if trimmed.chars().count() > MAX_PROMPT_CHARS {
        return Err(format!(
            "research prompt must be at most {MAX_PROMPT_CHARS} characters"
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
                "{role} must not delegate research runs"
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
}
