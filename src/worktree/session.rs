//! Session identity resolution for worktree leases (issue 305 Task 1,
//! literal fallback removed by issue 651 P4).
//!
//! A worktree lease is keyed by `(repo, issue, session)`. Before issue 305
//! the CLI fabricated the constant `phasegent` whenever a caller omitted
//! `--session`, so two concurrent AI sessions working on the same issue
//! collided on the same lease. Issue 651 P4 removes that literal fallback
//! after every managed call path moved to an explicit identity: a session
//! id now resolves from an explicit flag, then `PHASEGENT_SESSION_ID`,
//! and anything else is a structured `argument` failure instead of a
//! shared fabricated owner. Lease-owned operations (`worktree acquire`,
//! `worktree heartbeat`) fail clearly without an identity; `issue close`
//! keeps its intentionally optional `--worktree-session` attribution via
//! [`resolve_session_optional`] and never guesses a closer.
//!
//! Values are trimmed, rejected when blank, and bounded to
//! [`MAX_SESSION_CHARS`] so two distinct sessions can never silently
//! collapse into the same lease key.

use super::WorktreeError;

/// Environment variable the OpenCode workflow sets once per session and
/// reuses for every worktree call in that session.
pub(crate) const SESSION_ENV: &str = "PHASEGENT_SESSION_ID";

/// Upper bound on a session id. Longer values are rejected rather than
/// truncated so distinct sessions cannot collide.
pub(crate) const MAX_SESSION_CHARS: usize = 128;

/// Which input supplied the resolved session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionSource {
    Explicit,
    Environment,
}

/// A validated session id together with the source that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionContext {
    pub id: String,
    pub source: SessionSource,
}

/// Resolve the session id from `explicit`, then `PHASEGENT_SESSION_ID`.
///
/// A missing identity is a structured `argument` error naming both
/// spellings — there is no fabricated fallback owner (issue 651 P4).
/// Lease-owned operations fail fast with this error before any storage
/// or git work.
pub(crate) fn resolve_session(explicit: Option<&str>) -> Result<SessionContext, WorktreeError> {
    if explicit.is_some() {
        return resolve_session_with(explicit, None);
    }
    let environment = std::env::var(SESSION_ENV).ok();
    resolve_session_with(None, environment.as_deref())
}

/// Resolve an *optional* session id for attributions that must stay
/// ownerless when no identity is known (`issue close --worktree-session`,
/// issue 651 P4).
///
/// Returns `Ok(None)` when neither the explicit flag nor
/// `PHASEGENT_SESSION_ID` names a session, so the caller keeps its
/// plain unattributed behavior instead of guessing a closer. A present
/// but blank or overlong value is still a structured `argument` error.
pub(crate) fn resolve_session_optional(
    explicit: Option<&str>,
) -> Result<Option<SessionContext>, WorktreeError> {
    if explicit.is_some() {
        return resolve_session_with(explicit, None).map(Some);
    }
    match std::env::var(SESSION_ENV).ok() {
        None => Ok(None),
        Some(environment) => resolve_session_with(None, Some(&environment)).map(Some),
    }
}

/// Source-parameterised core so tests can exercise precedence without
/// mutating the process environment.
pub(crate) fn resolve_session_with(
    explicit: Option<&str>,
    environment: Option<&str>,
) -> Result<SessionContext, WorktreeError> {
    let (raw, source) = match (explicit, environment) {
        (Some(value), _) => (value, SessionSource::Explicit),
        (None, Some(value)) => (value, SessionSource::Environment),
        (None, None) => {
            return Err(WorktreeError::new(
                "argument",
                format!("session identity is required: pass --session or set {SESSION_ENV}"),
            ));
        }
    };
    Ok(SessionContext {
        id: normalize_session_id(raw)?,
        source,
    })
}

fn normalize_session_id(raw: &str) -> Result<String, WorktreeError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WorktreeError::new(
            "argument",
            "session id must not be empty",
        ));
    }
    if trimmed.chars().count() > MAX_SESSION_CHARS {
        return Err(WorktreeError::new(
            "argument",
            format!("session id must be at most {MAX_SESSION_CHARS} characters"),
        ));
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    #[test]
    fn resolve_session_with_prefers_explicit_over_environment() {
        let context = resolve_session_with(Some("explicit"), Some("environment")).unwrap();
        assert_eq!(context.id, "explicit");
        assert_eq!(context.source, SessionSource::Explicit);
    }

    #[test]
    fn resolve_session_with_uses_environment_when_explicit_absent() {
        let context = resolve_session_with(None, Some("environment")).unwrap();
        assert_eq!(context.id, "environment");
        assert_eq!(context.source, SessionSource::Environment);
    }

    #[test]
    fn resolve_session_with_fails_without_any_identity() {
        let error = resolve_session_with(None, None).unwrap_err();
        assert_eq!(error.kind, "argument");
        assert!(
            error.message.contains("--session") && error.message.contains(SESSION_ENV),
            "a missing identity must name both spellings: {error}"
        );
    }

    #[test]
    fn resolve_session_with_trims_surrounding_whitespace() {
        let context = resolve_session_with(Some("  spaced-session  "), None).unwrap();
        assert_eq!(context.id, "spaced-session");
        assert_eq!(context.source, SessionSource::Explicit);
    }

    #[test]
    fn resolve_session_rejects_blank_explicit_value() {
        let error = resolve_session_with(Some("   "), None).unwrap_err();
        assert_eq!(error.kind, "argument");
        assert!(error.message.contains("empty"), "unexpected: {error}");
    }

    #[test]
    fn resolve_session_rejects_blank_environment_value() {
        let error = resolve_session_with(None, Some("")).unwrap_err();
        assert_eq!(error.kind, "argument");
        assert!(error.message.contains("empty"), "unexpected: {error}");
    }

    #[test]
    fn resolve_session_rejects_overlong_value() {
        let overlong = "s".repeat(MAX_SESSION_CHARS + 1);
        let error = resolve_session_with(Some(&overlong), None).unwrap_err();
        assert_eq!(error.kind, "argument");
        assert!(error.message.contains("128"), "unexpected: {error}");
    }

    #[test]
    fn resolve_session_accepts_max_length_value() {
        let exact = "s".repeat(MAX_SESSION_CHARS);
        let context = resolve_session_with(Some(&exact), None).unwrap();
        assert_eq!(context.id.len(), MAX_SESSION_CHARS);
    }

    #[test]
    fn resolve_session_reads_environment_variable() {
        let _lock = lock_workflow_tests();
        let _env = EnvGuard::set(SESSION_ENV, "env-session");
        let context = resolve_session(None).unwrap();
        assert_eq!(context.id, "env-session");
        assert_eq!(context.source, SessionSource::Environment);
    }

    #[test]
    fn resolve_session_explicit_bypasses_environment() {
        let context = resolve_session(Some("explicit-session")).unwrap();
        assert_eq!(context.id, "explicit-session");
        assert_eq!(context.source, SessionSource::Explicit);
    }

    #[test]
    fn resolve_session_optional_stays_ownerless_without_identity() {
        // NOTE: `resolve_session_optional(None)` reads the real process
        // environment, so this arm only holds when `PHASEGENT_SESSION_ID`
        // is unset; the workflow lock serialises env mutation and no
        // test in this target sets it without a guard.
        let _lock = lock_workflow_tests();
        let previous = std::env::var_os(SESSION_ENV);
        // SAFETY: serialised by `lock_workflow_tests`; restored below.
        unsafe {
            std::env::remove_var(SESSION_ENV);
        }
        let result = resolve_session_optional(None).unwrap();
        if let Some(previous) = previous {
            // SAFETY: symmetric with the removal above.
            unsafe {
                std::env::set_var(SESSION_ENV, previous);
            }
        }
        assert_eq!(result, None);
        assert_eq!(
            resolve_session_optional(Some("explicit"))
                .unwrap()
                .expect("explicit session")
                .id,
            "explicit"
        );
    }

    #[test]
    fn resolve_session_optional_rejects_blank_values() {
        let error = resolve_session_optional(Some("   ")).unwrap_err();
        assert_eq!(error.kind, "argument");
    }
}
