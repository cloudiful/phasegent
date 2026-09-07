//! Bounded agent notification envelopes.
//!
//! Manual `notify send` constructs a [`NotificationIntent`] and hands
//! it to [`crate::notifications::fire`] which persists the intent
//! before any network work. Titles and bodies are truncated to fixed char limits
//! so storage rows and provider payloads stay small even when callers
//! pass long planning text or error strings.

use std::collections::BTreeMap;

/// Maximum title chars kept in a notification intent. Titles are
/// single-line summaries; longer input is truncated with an ellipsis.
pub const NOTIFICATION_TITLE_LIMIT: usize = 140;
/// Maximum body chars kept in a notification intent. Bodies carry the
/// bounded detail (issue id, phase, status, short error); longer input
/// is truncated with an ellipsis.
pub const NOTIFICATION_BODY_LIMIT: usize = 2000;

/// Structured notification kinds. One variant per manual event so
/// persistence and delivery stay explicit about why a message exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationEvent {
    /// Manual completion notice.
    Completion,
    /// Manual blocked-attention notice.
    BlockedAttention,
    /// Manual failure notice.
    Failure,
    /// Manual interruption-suspected notice.
    InterruptionSuspected,
    /// Manual publish-failure notice.
    PublishFailed,
}

impl NotificationEvent {
    /// Canonical snake_case literal used in storage, JSON, and CLI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::BlockedAttention => "blocked",
            Self::Failure => "failure",
            Self::InterruptionSuspected => "interruption_suspected",
            Self::PublishFailed => "publish_failed",
        }
    }

    /// Parse a CLI `--event` value. Accepts the canonical literals plus
    /// the `blocked_attention` alias for ergonomics.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "completion" | "completed" | "done" => Ok(Self::Completion),
            "blocked" | "blocked_attention" => Ok(Self::BlockedAttention),
            "failure" | "failed" => Ok(Self::Failure),
            "interruption_suspected" | "interruption" | "interrupted" | "unconfirmed" => {
                Ok(Self::InterruptionSuspected)
            }
            "publish_failed" | "publish-failed" | "publish-failure" | "publish" => {
                Ok(Self::PublishFailed)
            }
            _ => Err(format!(
                "invalid --event '{value}'; expected completion, blocked, failure, interruption_suspected, or publish_failed"
            )),
        }
    }
}

/// Structured intent persisted before delivery. `title`/`body` are
/// already bounded; `issue_id` is optional context kept as an integer
/// so JSON stays typed. `metadata` carries at most a few short
/// string pairs (phase, status, channel) and never secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationIntent {
    pub event: NotificationEvent,
    pub title: String,
    pub body: String,
    pub issue_id: Option<u64>,
    pub metadata: BTreeMap<String, String>,
}

impl NotificationIntent {
    /// Build a bounded intent. Title/body are single-lined and
    /// truncated; metadata values are bounded to 200 chars each and
    /// capped at 8 entries so a caller cannot blow up the row.
    pub fn new(
        event: NotificationEvent,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            event,
            title: bound_text(&title.into(), NOTIFICATION_TITLE_LIMIT),
            body: bound_text(&body.into(), NOTIFICATION_BODY_LIMIT),
            issue_id: None,
            metadata: BTreeMap::new(),
        }
    }

    /// Attach optional issue context.
    pub fn with_issue(mut self, issue_id: u64) -> Self {
        if issue_id > 0 {
            self.issue_id = Some(issue_id);
        }
        self
    }

    /// Attach one bounded metadata pair. Silently drops extras beyond
    /// 8 entries so manual sends stay cheap.
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        if self.metadata.len() >= 8 {
            return self;
        }
        let key = bound_text(&key.into(), 64);
        let value = bound_text(&value.into(), 200);
        if !key.is_empty() {
            self.metadata.insert(key, value);
        }
        self
    }

    /// Render the notifier envelope. Title stays as-is; body appends
    /// `issue #N` and `event=` context on separate lines only when
    /// present so the delivered text remains greppable without
    /// duplicating the title.
    pub fn to_message_envelope(&self) -> cloudiful_notifier::MessageEnvelope {
        let mut body = self.body.clone();
        if let Some(issue) = self.issue_id {
            body.push_str(&format!("\nissue #{issue}"));
        }
        body.push_str(&format!("\nevent={}", self.event.as_str()));
        let mut envelope =
            cloudiful_notifier::MessageEnvelope::new(body).with_title(self.title.clone());
        for (key, value) in &self.metadata {
            envelope
                .metadata
                .insert(key.clone(), serde_json::Value::String(value.clone()));
        }
        envelope.metadata.insert(
            "event".to_owned(),
            serde_json::Value::String(self.event.as_str().to_owned()),
        );
        if let Some(issue) = self.issue_id {
            envelope.metadata.insert(
                "issue_id".to_owned(),
                serde_json::Value::Number(serde_json::Number::from(issue)),
            );
        }
        envelope
    }
}

/// Single-line and truncate `raw` to `limit` chars. Control chars and
/// newlines become spaces, runs of spaces collapse, and truncation
/// appends `...` within the limit.
fn bound_text(raw: &str, limit: usize) -> String {
    let single = raw.replace(['\n', '\r'], " ");
    let cleaned: String = single.chars().filter(|c| !c.is_control()).collect();
    let mut collapsed = String::with_capacity(cleaned.len());
    let mut last_space = false;
    for ch in cleaned.trim().chars() {
        if ch.is_whitespace() {
            if !last_space {
                collapsed.push(' ');
            }
            last_space = true;
        } else {
            collapsed.push(ch);
            last_space = false;
        }
    }
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let keep = limit.saturating_sub(3);
    let truncated: String = collapsed.chars().take(keep).collect();
    format!("{truncated}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_through_literals() {
        for (event, literal) in [
            (NotificationEvent::Completion, "completion"),
            (NotificationEvent::BlockedAttention, "blocked"),
            (NotificationEvent::Failure, "failure"),
            (
                NotificationEvent::InterruptionSuspected,
                "interruption_suspected",
            ),
            (NotificationEvent::PublishFailed, "publish_failed"),
        ] {
            assert_eq!(event.as_str(), literal);
            assert_eq!(NotificationEvent::parse(literal).unwrap(), event);
        }
    }

    #[test]
    fn intent_truncates_long_title_and_body() {
        let intent = NotificationIntent::new(
            NotificationEvent::Completion,
            "t".repeat(500),
            "b".repeat(5000),
        );
        assert!(intent.title.chars().count() <= NOTIFICATION_TITLE_LIMIT);
        assert!(intent.body.chars().count() <= NOTIFICATION_BODY_LIMIT);
        assert!(intent.title.ends_with("..."));
    }

    #[test]
    fn envelope_carries_event_metadata_without_secrets() {
        let intent =
            NotificationIntent::new(NotificationEvent::Failure, "timer failed", "run abc failed")
                .with_issue(7)
                .with_meta("phase", "notifications");
        let envelope = intent.to_message_envelope();
        assert_eq!(envelope.title.as_deref(), Some("timer failed"));
        assert!(envelope.body.contains("issue #7"));
        assert_eq!(
            envelope.metadata.get("event").and_then(|v| v.as_str()),
            Some("failure")
        );
    }
}
