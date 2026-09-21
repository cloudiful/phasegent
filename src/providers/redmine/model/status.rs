use crate::providers::api::{ForgejoError, IssueSummary};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedmineIssueStatus {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub is_closed: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineIssueStatusCollection {
    #[serde(default)]
    pub(crate) issue_statuses: Vec<RedmineIssueStatus>,
}

/// Redmine tracker (for example `Bug` or `Feature`) as exposed by
/// `/trackers.json`. Issues reference trackers by id on create and update.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedmineTracker {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineTrackerCollection {
    #[serde(default)]
    pub(crate) trackers: Vec<RedmineTracker>,
}

/// Redmine project version (Roadmap milestone) as exposed by
/// `/projects/:id/versions.json`. Issues reference versions by id through
/// the native `fixed_version_id` planning field.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedmineVersion {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineVersionCollection {
    #[serde(default)]
    pub(crate) versions: Vec<RedmineVersion>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

/// Stable identifier for the canonical phase transition policy. It is
/// emitted by every status capability and transition error so an AI or
/// operator can tell phasegent's own guidance apart from a server-side
/// Redmine workflow rejection.
pub const STATUS_POLICY_SOURCE: &str = "phasegent/canonical-phase-workflow@v1";

/// Explicit caveat attached to every policy answer: the Redmine
/// installation's workflow permissions and custom statuses remain
/// authoritative, so the policy never claims universal permission.
pub const STATUS_POLICY_CAVEAT: &str = "Policy guidance only: the Redmine server workflow, role permissions, and custom statuses are authoritative and may allow or reject transitions this policy does not describe.";

/// Canonical phase transition graph. This table is the single source of
/// truth for the workflow; the OpenCode plugin and the orchestrator
/// prompt must query phasegent instead of restating it.
///
/// The row order is the auto-route preference: a bare `status transition`
/// takes the first allowed next status of the current row. `Resolved` is
/// the non-terminal "AI work finished, awaiting the user's verification"
/// state, so its first edge is the verified terminal `Closed`; the
/// `In Progress` edge stays available as an explicitly targeted resume
/// after a reviewed phase, which keeps multi-phase work reachable
/// without making it the default.
const STATUS_TRANSITIONS: &[(&str, &[&str])] = &[
    ("New", &["In Progress", "Cancelled"]),
    ("In Progress", &["In Review", "Blocked", "Cancelled"]),
    (
        "In Review",
        &["Resolved", "Changes Requested", "Blocked", "Cancelled"],
    ),
    (
        "Changes Requested",
        &["In Progress", "Blocked", "Cancelled"],
    ),
    ("Blocked", &["In Progress", "Cancelled"]),
    ("Resolved", &["Closed", "In Progress"]),
    ("Closed", &[]),
    ("Cancelled", &[]),
];

/// Resolve an installation status name to its canonical spelling.
/// Matching is case-insensitive and whitespace-tolerant so an
/// installation that stores `in progress` still maps onto the policy;
/// anything else is treated as a custom, server-controlled status.
pub fn canonical_status_name(value: &str) -> Option<&'static str> {
    let needle = value.trim().to_ascii_lowercase();
    STATUS_TRANSITIONS
        .iter()
        .map(|(name, _)| *name)
        .find(|name| name.to_ascii_lowercase() == needle)
}

/// Policy-allowed next statuses for a canonical status, or `None` when
/// the status is not part of the canonical graph.
pub fn canonical_allowed_next(value: &str) -> Option<&'static [&'static str]> {
    let canonical = canonical_status_name(value)?;
    STATUS_TRANSITIONS
        .iter()
        .find(|(name, _)| *name == canonical)
        .map(|(_, next)| *next)
}

/// Outcome of evaluating one transition against the canonical policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionVerdict {
    /// Current and target are the same status; the caller must treat the
    /// transition as an idempotent no-op instead of issuing a PUT.
    NoOp,
    /// Both statuses are canonical and the edge exists in the graph.
    Allowed,
    /// Both statuses are canonical and the edge does not exist.
    Forbidden {
        allowed_next: &'static [&'static str],
    },
    /// At least one side is unknown or custom, so the policy cannot
    /// judge the transition and the server decides.
    Advisory { reason: String },
}

/// Evaluate `current -> target` against the canonical policy. Same-name
/// transitions short-circuit to `NoOp` even for custom statuses because
/// re-applying the current status is always a no-op.
pub fn evaluate_transition(current: &str, target: &str) -> TransitionVerdict {
    let (Some(from), Some(to)) = (
        canonical_status_name(current),
        canonical_status_name(target),
    ) else {
        if current.trim().eq_ignore_ascii_case(target.trim()) {
            return TransitionVerdict::NoOp;
        }
        let unknown = if canonical_status_name(current).is_none() {
            format!("current status '{current}'")
        } else {
            format!("target status '{target}'")
        };
        return TransitionVerdict::Advisory {
            reason: format!(
                "{unknown} is not part of the canonical policy; the Redmine server decides this transition"
            ),
        };
    };
    if from == to {
        return TransitionVerdict::NoOp;
    }
    let allowed_next = canonical_allowed_next(from).unwrap_or(&[]);
    if allowed_next.contains(&to) {
        TransitionVerdict::Allowed
    } else {
        TransitionVerdict::Forbidden { allowed_next }
    }
}

/// One status as reported back to the caller. `canonical` distinguishes
/// a policy-known status from a custom, server-controlled one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StatusRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_closed: Option<bool>,
    pub canonical: bool,
}

impl StatusRef {
    pub fn from_installation(status: &RedmineIssueStatus) -> Self {
        Self {
            id: Some(status.id),
            name: status.name.clone(),
            is_closed: Some(status.is_closed),
            canonical: canonical_status_name(&status.name).is_some(),
        }
    }

    pub(crate) fn from_issue_status(status: &super::issue::RedmineStatus) -> Self {
        Self {
            id: status.known_id(),
            name: status.name.clone(),
            is_closed: status.is_closed,
            canonical: canonical_status_name(&status.name).is_some(),
        }
    }
}

/// JSON payload of `status next <ISSUE>`: the issue's current status,
/// the policy-allowed next statuses resolved to installation-specific
/// ids, the policy identifier, and the explicit server caveat.
#[derive(Clone, Debug, Serialize)]
pub struct StatusNextReport {
    pub issue: u64,
    pub current: StatusRef,
    pub allowed_next: Vec<StatusRef>,
    /// Policy statuses that this installation does not define. They are
    /// reported by name so a renamed workflow is visible instead of
    /// silently dropped.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_next_missing_on_server: Vec<String>,
    pub policy_source: &'static str,
    /// `true` when the current status is outside the canonical graph, so
    /// `allowed_next` cannot be derived from policy at all.
    pub advisory: bool,
    pub caveat: &'static str,
    pub recovery: String,
}

/// JSON payload of a policy-checked transition. `changed` is `false`
/// for the idempotent same-status case so a caller can distinguish a
/// no-op from an actual server update.
#[derive(Debug, Serialize)]
pub struct StatusTransitionOutcome {
    pub issue: u64,
    pub changed: bool,
    pub from: StatusRef,
    pub to: StatusRef,
    pub policy_source: &'static str,
    pub advisory: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caveat: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_summary: Option<IssueSummary>,
}

/// Policy-allowed next status names for a transition the canonical policy
/// forbids, or `None` when the verdict is anything else. Phase 1
/// (issue 443) structured-`Forbidden` support: the CLI attaches this to
/// the advance error JSON instead of leaving the guidance in the message
/// text only. Pure policy lookup: no provider or network access.
pub fn forbidden_allowed_next_names(current: &str, target: &str) -> Option<Vec<String>> {
    match evaluate_transition(current, target) {
        TransitionVerdict::Forbidden { allowed_next } => {
            Some(allowed_next.iter().map(|name| (*name).to_owned()).collect())
        }
        _ => None,
    }
}

/// Extract `(current, target)` status names from a policy-preflight
/// rejection message. Both the Redmine
/// (`current status 'C' -> target status 'T' ...`) and the Local
/// (`current status 'C' -> 'T' ...`) wordings are accepted: the current
/// name is the first quoted segment after `current status`, the target
/// is the next quoted segment. Returns `None` for any other message so
/// server-side rejections keep their legacy shape.
pub fn parse_forbidden_transition(message: &str) -> Option<(String, String)> {
    const MARKER: &str = "current status '";
    let start = message.find(MARKER)? + MARKER.len();
    let rest = &message[start..];
    let end = rest.find('\'')?;
    let current = rest[..end].to_owned();
    let after = &rest[end + 1..];
    let target_start = after.find('\'')? + 1;
    let target_rest = &after[target_start..];
    let target_end = target_rest.find('\'')?;
    let target = target_rest[..target_end].to_owned();
    if current.trim().is_empty() || target.trim().is_empty() {
        return None;
    }
    Some((current, target))
}

/// Build the Phase 1 structured `Forbidden` error JSON for a
/// policy-preflight rejection: the legacy `kind`/`operation`/`message`
/// triple plus machine-readable `current`, `target`, `allowed_next`,
/// and `policy_source`. Returns `None` when the error is not a canonical
/// policy rejection so every other failure keeps its legacy shape. The
/// fields are additive, so existing `kind`-based assertions keep passing.
pub fn structured_forbidden_json(error: &ForgejoError) -> Option<serde_json::Value> {
    let (operation, message) = match error {
        ForgejoError::Request { operation, message } => (operation, message),
        _ => return None,
    };
    if !message.contains("transition rejected before any write") {
        return None;
    }
    let (current, target) = parse_forbidden_transition(message)?;
    let allowed_next = forbidden_allowed_next_names(&current, &target)?;
    Some(serde_json::json!({
        "kind": "request",
        "operation": operation,
        "message": message,
        "current": current,
        "target": target,
        "allowed_next": allowed_next,
        "policy_source": STATUS_POLICY_SOURCE,
    }))
}

/// Ordered `advance` steps that walk the canonical policy from
/// `current` to `Resolved`, the staging state before the final close
/// PUT. Phase 3 (issue 443) close-climb support: `close` retries a
/// workflow-rejected direct `PUT close_id` by stepping through these
/// names with `advance_issue_status` and then retrying the close PUT.
/// Empty means no climb is possible (already `Resolved`, terminal, or
/// custom): the caller must surface a structured `Forbidden` with
/// `status next` recovery instead. Pure policy lookup.
pub fn close_climb_steps(current: &str) -> Vec<&'static str> {
    match canonical_status_name(current) {
        Some("New") => vec!["In Progress", "In Review", "Resolved"],
        Some("In Progress") => vec!["In Review", "Resolved"],
        Some("In Review") => vec!["Resolved"],
        Some("Changes Requested") => vec!["In Progress", "In Review", "Resolved"],
        Some("Blocked") => vec!["In Progress", "In Review", "Resolved"],
        Some("Resolved") | Some("Closed") | Some("Cancelled") => vec![],
        _ => vec!["Resolved"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_lookup_returns_names_only_for_illegal_canonical_edges() {
        assert_eq!(
            forbidden_allowed_next_names("Resolved", "In Review"),
            Some(vec!["Closed".to_owned(), "In Progress".to_owned()])
        );
        assert_eq!(
            forbidden_allowed_next_names("Closed", "In Progress"),
            Some(vec![])
        );
        assert_eq!(forbidden_allowed_next_names("New", "In Progress"), None);
        assert_eq!(forbidden_allowed_next_names("New", "New"), None);
        assert_eq!(forbidden_allowed_next_names("Triaged", "In Progress"), None);
    }

    /// A bare `status transition` takes the first allowed next status of
    /// the current row, so the canonical auto-route walks
    /// `In Review -> Resolved -> Closed`; the resume edge back to
    /// `In Progress` stays allowed behind an explicit target.
    #[test]
    fn bare_auto_route_walks_in_review_to_resolved_to_closed() {
        assert_eq!(
            canonical_allowed_next("In Review").and_then(|next| next.first()),
            Some(&"Resolved"),
            "a bare transition from In Review must land on Resolved"
        );
        assert_eq!(
            canonical_allowed_next("Resolved").and_then(|next| next.first()),
            Some(&"Closed"),
            "a bare transition from Resolved must land on the verified terminal status"
        );
        assert!(
            canonical_allowed_next("Resolved").is_some_and(|next| next.contains(&"In Progress")),
            "the explicitly targeted resume edge must stay allowed"
        );
    }

    #[test]
    fn forbidden_parser_accepts_both_provider_wordings() {
        let redmine = "transition rejected before any write: current status 'Resolved' -> target status 'In Review' is not allowed by policy phasegent/canonical-phase-workflow@v1; allowed_next=[Closed, In Progress]; Policy guidance only recovery: phasegent --role orchestrator --provider redmine status next 7";
        assert_eq!(
            parse_forbidden_transition(redmine),
            Some(("Resolved".to_owned(), "In Review".to_owned()))
        );
        let local = "transition rejected before any write: current status 'New' -> 'Closed' is not allowed by policy phasegent/canonical-phase-workflow@v1; allowed_next=[In Progress, Cancelled]";
        assert_eq!(
            parse_forbidden_transition(local),
            Some(("New".to_owned(), "Closed".to_owned()))
        );
        assert_eq!(parse_forbidden_transition("Redmine did not confirm"), None);
        assert_eq!(parse_forbidden_transition("current status '' -> 'X'"), None);
    }

    #[test]
    fn structured_forbidden_json_round_trips_a_preflight_rejection() {
        let message = "transition rejected before any write: current status 'Resolved' -> target status 'In Review' is not allowed by policy phasegent/canonical-phase-workflow@v1; allowed_next=[Closed, In Progress]; Policy guidance only recovery: phasegent --role orchestrator --provider redmine status next 7";
        let error = ForgejoError::request("issue status advance", message.to_owned());
        let json = structured_forbidden_json(&error).expect("preflight rejection must map");
        assert_eq!(json["kind"], "request");
        assert_eq!(json["operation"], "issue status advance");
        assert_eq!(json["message"], message);
        assert_eq!(json["current"], "Resolved");
        assert_eq!(json["target"], "In Review");
        assert_eq!(
            json["allowed_next"],
            serde_json::json!(["Closed", "In Progress"])
        );
        assert_eq!(json["policy_source"], STATUS_POLICY_SOURCE);
    }

    #[test]
    fn structured_forbidden_json_rejects_server_side_and_non_request_errors() {
        let server = ForgejoError::request(
            "issue status advance",
            "boom; current status 'In Progress' -> target status 'In Review'; server rejected a policy-allowed or custom transition, so the Redmine workflow is authoritative; recovery: phasegent --role orchestrator --provider redmine status next 7".to_owned(),
        );
        assert!(structured_forbidden_json(&server).is_none());
        let config = ForgejoError::config("issue number must be greater than zero");
        assert!(structured_forbidden_json(&config).is_none());
    }

    #[test]
    fn close_climb_steps_follow_policy_to_resolved() {
        assert_eq!(
            close_climb_steps("New"),
            vec!["In Progress", "In Review", "Resolved"]
        );
        assert_eq!(
            close_climb_steps("In Progress"),
            vec!["In Review", "Resolved"]
        );
        assert_eq!(close_climb_steps("In Review"), vec!["Resolved"]);
        assert_eq!(
            close_climb_steps("Blocked"),
            vec!["In Progress", "In Review", "Resolved"]
        );
        assert!(close_climb_steps("Resolved").is_empty());
        assert!(close_climb_steps("Closed").is_empty());
        assert!(close_climb_steps("Cancelled").is_empty());
        assert_eq!(close_climb_steps("Triaged"), vec!["Resolved"]);
    }
}
