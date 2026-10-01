//! Structured errors for the ACP adapter.
//!
//! Messages are bounded, control-character free, and never embed
//! process environment or credential material. An [`AgentError`] is the
//! sink every failure reaches, and two of its inputs are agent-owned
//! text rather than this client's: the child's stderr tail and the
//! `error.message` the child puts in a JSON-RPC error response. The
//! child is a process that legitimately holds the model's credential, so
//! either could carry a secret — and the message is persisted in the run
//! ledger and returned to a caller. Redaction therefore happens in
//! [`AgentError::new`], the single point where any text becomes an error
//! message, so no call site can attach unredacted agent output.
//!
//! Redaction runs before [`sanitize`] on purpose: truncating first could
//! cut a secret in half and leave a fragment that matches no rule.

use super::redact;

/// Upper bound for any message carried in an [`AgentError`].
pub const MAX_MESSAGE_CHARS: usize = 300;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentErrorKind {
    Spawn,
    Protocol,
    Timeout,
    Cancelled,
    Closed,
    Negotiation,
}

impl AgentErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Protocol => "protocol",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Closed => "closed",
            Self::Negotiation => "negotiation",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentError {
    pub kind: AgentErrorKind,
    pub message: String,
}

impl AgentError {
    pub fn new(kind: AgentErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: sanitize(&redact::redact(&message.into())),
        }
    }

    pub fn spawn(message: impl Into<String>) -> Self {
        Self::new(AgentErrorKind::Spawn, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(AgentErrorKind::Protocol, message)
    }

    pub fn timeout() -> Self {
        Self::new(AgentErrorKind::Timeout, "explorer run exceeded its timeout")
    }

    pub fn cancelled() -> Self {
        Self::new(AgentErrorKind::Cancelled, "explorer run was cancelled")
    }

    pub fn closed() -> Self {
        Self::new(
            AgentErrorKind::Closed,
            "explorer process closed unexpectedly",
        )
    }

    pub fn negotiation(message: impl Into<String>) -> Self {
        Self::new(AgentErrorKind::Negotiation, message)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"kind": self.kind.as_str(), "message": self.message})
    }
}

/// Bound a message and strip control characters so process output can
/// never flood a log, a result, or a persisted row.
pub fn sanitize(raw: &str) -> String {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_MESSAGE_CHARS {
        return collapsed;
    }
    let keep = MAX_MESSAGE_CHARS.saturating_sub(3);
    format!("{}...", collapsed.chars().take(keep).collect::<String>())
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for AgentError {}

/// Convenience alias used across the adapter.
pub type AgentResult<T> = Result<T, AgentError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_bounds_and_strips_control_characters() {
        assert_eq!(sanitize("a\x07b\n c"), "ab c");
        let long = "x".repeat(MAX_MESSAGE_CHARS + 50);
        let cleaned = sanitize(&long);
        assert_eq!(cleaned.chars().count(), MAX_MESSAGE_CHARS);
        assert!(cleaned.ends_with("..."));
    }

    #[test]
    fn error_kinds_and_json_shape_are_stable() {
        let error = AgentError::negotiation("model not advertised");
        assert_eq!(error.kind.as_str(), "negotiation");
        assert_eq!(
            error.to_json(),
            serde_json::json!({"kind": "negotiation", "message": "model not advertised"})
        );
        assert_eq!(AgentError::timeout().kind.as_str(), "timeout");
        assert_eq!(AgentError::cancelled().kind.as_str(), "cancelled");
        assert_eq!(AgentError::closed().kind.as_str(), "closed");
    }

    #[test]
    fn every_message_is_redacted_at_the_sink() {
        // The two agent-owned inputs are the stderr tail (attached by
        // `with_diagnostics`) and the `error.message` of a JSON-RPC error
        // response. Both land here, so both are covered by construction
        // rather than by remembering to redact at each call site.
        for raw in [
            "acp error -32601: upstream refused Authorization: Bearer sk-live-VALUE0123456789",
            "acp error -32601: request failed api_key=abcd1234efgh5678",
            "acp error -32601: dial https://user:s3cr3tpw@example.com/api",
        ] {
            let error = AgentError::protocol(raw);
            assert!(error.message.contains("[redacted]"), "{raw}");
        }
        // Truncation must not run first: a cut secret would leave a
        // fragment that matches no rule.
        let long_prefix = "acp error -32601: ".to_owned() + &"x".repeat(MAX_MESSAGE_CHARS);
        let error = AgentError::protocol(format!("{long_prefix} sk-live-VALUE0123456789"));
        assert!(!error.message.contains("sk-live"), "{}", error.message);
    }

    #[test]
    fn redaction_keeps_ordinary_diagnostics_readable() {
        let raw = "acp error -32601: session/prompt failed for /home/dev/wt/src/main.rs";
        assert_eq!(AgentError::protocol(raw).message, raw);
    }
}
