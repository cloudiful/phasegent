//! Session identity resolution for worktree leases (issue 305 Task 1).
//!
//! A worktree lease is keyed by `(repo, issue, session)`. Before issue 305
//! the CLI fabricated the constant `phasegent` whenever a caller omitted
//! `--session`, so two concurrent AI sessions working on the same issue
//! collided on the same lease. This module resolves a session id from an
//! explicit flag, then `PHASEGENT_SESSION_ID`, then the legacy `phasegent`
//! fallback, and tags which source won so the CLI can emit a migration
//! warning on stderr without changing the stdout JSON envelope.
//!
//! Values are trimmed, rejected when blank, and bounded to
//! [`MAX_SESSION_CHARS`] so two distinct sessions can never silently
//! collapse into the same lease key.

use super::WorktreeError;

/// Environment variable the OpenCode workflow sets once per session and
/// reuses for every worktree call in that session.
pub(crate) const SESSION_ENV: &str = "PHASEGENT_SESSION_ID";

/// Legacy constant session label kept for backwards compatibility with
/// callers that predate the environment variable.
pub(crate) const LEGACY_SESSION_ID: &str = "phasegent";

/// Upper bound on a session id. Longer values are rejected rather than
/// truncated so distinct sessions cannot collide.
pub(crate) const MAX_SESSION_CHARS: usize = 128;

/// Which input supplied the resolved session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionSource {
    Explicit,
    Environment,
    LegacyFallback,
}

/// A validated session id together with the source that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionContext {
    pub id: String,
    pub source: SessionSource,
}

impl SessionContext {
    /// Migration warning for the legacy fallback only. An explicit or
    /// environment-derived session keeps the common path silent.
    pub(crate) fn legacy_warning(&self) -> Option<String> {
        match self.source {
            SessionSource::LegacyFallback => Some(format!(
                "no --session or {SESSION_ENV} provided; using legacy session '{LEGACY_SESSION_ID}', \
                 which concurrent sessions may share. Export {SESSION_ENV} to isolate this session."
            )),
            SessionSource::Explicit | SessionSource::Environment => None,
        }
    }
}

/// Resolve the session id from `explicit`, then `PHASEGENT_SESSION_ID`, then
/// the legacy `phasegent` fallback.
pub(crate) fn resolve_session(explicit: Option<&str>) -> Result<SessionContext, WorktreeError> {
    if explicit.is_some() {
        return resolve_session_with(explicit, None);
    }
    let environment = std::env::var(SESSION_ENV).ok();
    resolve_session_with(None, environment.as_deref())
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
            return Ok(SessionContext {
                id: LEGACY_SESSION_ID.to_owned(),
                source: SessionSource::LegacyFallback,
            });
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
    fn resolve_session_with_falls_back_to_legacy_when_both_absent() {
        let context = resolve_session_with(None, None).unwrap();
        assert_eq!(context.id, LEGACY_SESSION_ID);
        assert_eq!(context.source, SessionSource::LegacyFallback);
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
    fn legacy_warning_only_for_fallback_source() {
        assert!(
            resolve_session_with(None, None)
                .unwrap()
                .legacy_warning()
                .is_some()
        );
        assert!(
            resolve_session_with(Some("explicit"), None)
                .unwrap()
                .legacy_warning()
                .is_none()
        );
        assert!(
            resolve_session_with(None, Some("environment"))
                .unwrap()
                .legacy_warning()
                .is_none()
        );
    }
}
