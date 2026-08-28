use crate::providers::api::IssueSummary;
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
/// `Resolved` is a per-phase state, not a task-final one: it marks one
/// reviewed phase. It therefore carries two distinct outgoing edges —
/// `In Progress` is the phase-continuation edge taken after that phase's
/// checkpoint/push when the plan still has remaining phases, and
/// `Closed` is the task-final edge taken only after the last
/// checkpoint/push. Omitting the continuation edge would make a
/// multi-phase task impossible to advance.
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
    ("Resolved", &["In Progress", "Closed"]),
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
