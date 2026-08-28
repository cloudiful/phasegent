use crate::providers::api::{CommentOutput, ForgejoError, IssueSummary};
use crate::providers::config::RedmineProvider;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedmineProject {
    pub id: u64,
    pub name: String,
    pub identifier: String,
    #[serde(
        default,
        deserialize_with = "deserialize_null_as_empty_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_public: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherit_members: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_on: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_on: Option<String>,
}

fn deserialize_null_as_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|value| value.unwrap_or_default())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedmineIssueStatus {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub is_closed: bool,
}

/// Redmine tracker (for example `Bug` or `Feature`) as exposed by
/// `/trackers.json`. Issues reference trackers by id on create and update.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedmineTracker {
    pub id: u64,
    pub name: String,
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

/// A time-entry activity exposed by Redmine's enumeration API.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct RedmineTimeEntryActivity {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
}

impl RedmineProvider {
    /// Resolve a time-entry activity without ever guessing. The preferred
    /// names are exact and case-sensitive; only a single default activity is
    /// acceptable. Duplicate preferred names and multiple defaults are
    /// configuration errors so hours cannot be silently misclassified.
    pub fn select_time_entry_activity(
        activities: &[RedmineTimeEntryActivity],
    ) -> Result<&RedmineTimeEntryActivity, ForgejoError> {
        for name in ["AI automation", "Development"] {
            let matches = activities
                .iter()
                .filter(|activity| activity.name == name)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [] => {}
                [activity] => {
                    if activity.id == 0 {
                        return Err(ForgejoError::config(format!(
                            "Redmine time-entry activity name '{name}' has id zero"
                        )));
                    }
                    return Ok(activity);
                }
                _ => {
                    return Err(ForgejoError::config(format!(
                        "Redmine time-entry activity name '{name}' is ambiguous"
                    )));
                }
            }
        }

        let defaults = activities
            .iter()
            .filter(|activity| activity.is_default)
            .collect::<Vec<_>>();
        match defaults.as_slice() {
            [activity] if activity.id > 0 => Ok(activity),
            [] => Err(ForgejoError::config(
                "Redmine has no exact AI automation or Development activity and no default time-entry activity",
            )),
            _ => Err(ForgejoError::config(
                "Redmine has multiple default time-entry activities; set an exact AI automation or Development activity",
            )),
        }
    }
}

impl NamedRecord for RedmineTimeEntryActivity {
    fn name(&self) -> &str {
        &self.name
    }
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

    pub(crate) fn from_issue_status(status: &RedmineStatus) -> Self {
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

impl RedmineProvider {
    /// Resolve an issue status by validated numeric id or exact name.
    /// Numeric zero is rejected; ambiguous names fail with an actionable
    /// error because a status update must land on exactly one status.
    pub fn select_status_by_value<'a>(
        statuses: &'a [RedmineIssueStatus],
        value: &str,
    ) -> Result<&'a RedmineIssueStatus, ForgejoError> {
        if let Ok(id) = value.parse::<u64>() {
            if id == 0 {
                return Err(ForgejoError::config(
                    "Redmine status id must be greater than zero",
                ));
            }
            return statuses
                .iter()
                .find(|status| status.id == id)
                .ok_or_else(|| {
                    ForgejoError::config(format!("Redmine status id {id} was not found"))
                });
        }
        select_by_name(statuses.iter(), value, "status")
    }

    /// Resolve a tracker by validated numeric id or exact name using the
    /// same rules as status resolution so callers cannot silently target
    /// an ambiguous or unknown tracker.
    pub fn select_tracker<'a>(
        trackers: &'a [RedmineTracker],
        value: &str,
    ) -> Result<&'a RedmineTracker, ForgejoError> {
        if let Ok(id) = value.parse::<u64>() {
            if id == 0 {
                return Err(ForgejoError::config(
                    "Redmine tracker id must be greater than zero",
                ));
            }
            return trackers
                .iter()
                .find(|tracker| tracker.id == id)
                .ok_or_else(|| {
                    ForgejoError::config(format!("Redmine tracker id {id} was not found"))
                });
        }
        select_by_name(trackers.iter(), value, "tracker")
    }

    /// Resolve a project version by validated numeric id or exact name so
    /// `--fixed-version` lands on exactly one version of the configured
    /// project. Zero ids are rejected like every other selector.
    pub fn select_version<'a>(
        versions: &'a [RedmineVersion],
        value: &str,
    ) -> Result<&'a RedmineVersion, ForgejoError> {
        if let Ok(id) = value.parse::<u64>() {
            if id == 0 {
                return Err(ForgejoError::config(
                    "Redmine version id must be greater than zero",
                ));
            }
            return versions
                .iter()
                .find(|version| version.id == id)
                .ok_or_else(|| {
                    ForgejoError::config(format!("Redmine version id {id} was not found"))
                });
        }
        select_by_name(versions.iter(), value, "version")
    }
}

fn select_by_name<'a, I, T>(items: I, value: &str, kind: &str) -> Result<&'a T, ForgejoError>
where
    I: IntoIterator<Item = &'a T>,
    T: NamedRecord,
{
    let matches = items
        .into_iter()
        .filter(|item| item.name() == value)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [item] => Ok(item),
        [] => Err(ForgejoError::config(format!(
            "Redmine {kind} name '{value}' was not found"
        ))),
        _ => Err(ForgejoError::config(format!(
            "Redmine {kind} name '{value}' is ambiguous"
        ))),
    }
}

trait NamedRecord {
    fn name(&self) -> &str;
}

impl NamedRecord for RedmineIssueStatus {
    fn name(&self) -> &str {
        &self.name
    }
}

impl NamedRecord for RedmineTracker {
    fn name(&self) -> &str {
        &self.name
    }
}

impl NamedRecord for RedmineVersion {
    fn name(&self) -> &str {
        &self.name
    }
}

/// Default Redmine role for the orchestrator user identified by the
/// `orchestrator` API key.
pub const DEFAULT_REDMINE_ROLE_ORCHESTRATOR: &str = "Maintainer";
/// Default Redmine role for the executor user identified by the `executor`
/// API key.
pub const DEFAULT_REDMINE_ROLE_EXECUTOR: &str = "Developer";
/// Default Redmine role for the reviewer user identified by the `reviewer`
/// API key.
pub const DEFAULT_REDMINE_ROLE_REVIEWER: &str = "Reporter";

#[derive(Debug)]
pub struct RedmineBootstrap {
    pub project: RedmineProject,
    pub close_status: RedmineIssueStatus,
    pub created: bool,
}

/// Outcome of reconciling a single user's direct project membership. The
/// `status` mirrors the bootstrap reconciliation vocabulary
/// (`added`/`updated`/`existing`/`warning`) so callers can decide whether the
/// workflow is ready.
#[derive(Debug)]
#[allow(dead_code)]
pub struct RedmineUserMembershipOutcome {
    pub user_id: u64,
    pub user_login: String,
    pub role_id: u64,
    pub role_name: String,
    pub status: String,
    pub warning: Option<String>,
}

/// Identity of the user behind a role-scoped Redmine API key. Returned by
/// `/users/current.json`; used to bind bootstrap output to a concrete user
/// rather than the opaque API key.
#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct RedmineCurrentUser {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) login: String,
    #[serde(default)]
    pub(crate) firstname: String,
    #[serde(default)]
    pub(crate) lastname: String,
    #[serde(default)]
    pub(crate) mail: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineRoleCollection {
    #[serde(default)]
    pub(crate) roles: Vec<RedmineRole>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineMembershipCollection {
    #[serde(default)]
    pub(crate) memberships: Vec<RedmineMembership>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RedmineRole {
    pub(crate) id: u64,
    pub(crate) name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineMembership {
    pub(crate) id: u64,
    pub(crate) user: Option<RedmineMembershipUser>,
    #[serde(default)]
    pub(crate) roles: Vec<RedmineMembershipRole>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct RedmineMembershipUser {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) login: String,
    #[serde(default)]
    pub(crate) name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineMembershipRole {
    pub(crate) id: u64,
}

/// Request payload for adding a new user membership.
#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewUserMembership {
    pub(crate) membership: RedmineNewUserMembershipFields,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewUserMembershipFields {
    pub(crate) user_id: u64,
    pub(crate) role_ids: Vec<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineCurrentUserResponse {
    pub(crate) user: RedmineCurrentUser,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineUpdateMembership {
    pub(crate) membership: RedmineUpdateMembershipFields,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineUpdateMembershipFields {
    pub(crate) role_ids: Vec<u64>,
}

impl RedmineProvider {
    pub fn find_project(&self, identifier: &str) -> Result<Option<RedmineProject>, ForgejoError> {
        let response: Option<RedmineProjectResponse> = self.http.get_optional(
            &format!("projects/{identifier}.json"),
            &[],
            "project lookup",
        )?;
        Ok(response.and_then(|response| {
            let project = response.project;
            (project.identifier == identifier).then_some(project)
        }))
    }

    pub fn select_close_status<'a>(
        statuses: &'a [RedmineIssueStatus],
        close_status_id: Option<&str>,
        close_status_name: Option<&str>,
    ) -> Result<&'a RedmineIssueStatus, ForgejoError> {
        if let Some(value) = close_status_id {
            let id = value
                .parse::<u64>()
                .map_err(|_| ForgejoError::config("Redmine close status id must be numeric"))?;
            if id == 0 {
                return Err(ForgejoError::config(
                    "Redmine close status id must be greater than zero",
                ));
            }
            return match statuses.iter().find(|status| status.id == id) {
                None => Err(ForgejoError::config(format!(
                    "Redmine status id {id} was not found"
                ))),
                Some(status) if !status.is_closed => Err(ForgejoError::config(format!(
                    "Redmine status id {id} was found but is not closed"
                ))),
                Some(status) => Ok(status),
            };
        }
        if let Some(name) = close_status_name {
            let matches = statuses
                .iter()
                .filter(|status| status.name == name)
                .collect::<Vec<_>>();
            return match matches.as_slice() {
                [status] if status.is_closed => Ok(status),
                [] => Err(ForgejoError::config(format!(
                    "Redmine status name '{name}' was not found"
                ))),
                [_] => Err(ForgejoError::config(format!(
                    "Redmine status name '{name}' was found but is not closed"
                ))),
                _ => Err(ForgejoError::config(format!(
                    "Redmine status name '{name}' is ambiguous"
                ))),
            };
        }
        let mut closed = statuses.iter().filter(|status| status.is_closed);
        let status = closed
            .next()
            .ok_or_else(|| ForgejoError::config("Redmine has no closed issue status"))?;
        if closed.next().is_some() {
            return Err(ForgejoError::config(
                "Redmine has multiple closed issue statuses; use --close-status-id or --close-status-name",
            ));
        }
        Ok(status)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineProjectCollection {
    #[serde(default)]
    pub(crate) projects: Vec<RedmineProject>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineProjectResponse {
    pub(crate) project: RedmineProject,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineIssueStatusCollection {
    #[serde(default)]
    pub(crate) issue_statuses: Vec<RedmineIssueStatus>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineTrackerCollection {
    #[serde(default)]
    pub(crate) trackers: Vec<RedmineTracker>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineVersionCollection {
    #[serde(default)]
    pub(crate) versions: Vec<RedmineVersion>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineTimeEntryActivityCollection {
    #[serde(default)]
    pub(crate) time_entry_activities: Vec<RedmineTimeEntryActivity>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineIssueResponse {
    pub(crate) issue: RedmineIssue,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineIssueCollection {
    #[serde(default)]
    pub(crate) issues: Vec<RedmineIssue>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineIssue {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) subject: String,
    #[serde(default)]
    pub(crate) description: String,
    pub(crate) status: Option<RedmineStatus>,
    #[serde(default)]
    pub(crate) journals: Vec<RedmineJournal>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineStatus {
    /// Server-assigned status id. Missing or zero values indicate a
    /// legacy response shape (older Redmine releases, mock fixtures)
    /// where the caller cannot verify a status transition by id and must
    /// rely on the `is_closed` flag or a follow-up `GET` instead.
    #[serde(default)]
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) name: String,
    pub(crate) is_closed: Option<bool>,
}

impl RedmineStatus {
    /// `Some(id)` when the response carries a usable status id, `None`
    /// otherwise. The provider-level status verification treats the
    /// `None` case as "cannot verify by id" and falls back to either
    /// `is_closed` (for close) or a follow-up `GET`.
    pub(crate) fn known_id(&self) -> Option<u64> {
        if self.id > 0 { Some(self.id) } else { None }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineJournal {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) notes: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineErrorResponse {
    #[serde(default)]
    pub(crate) errors: Vec<Value>,
}

/// A Time Entry returned by Redmine. Optional nested fields let list calls
/// remain compatible with installations that omit one or more projections.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RedmineTimeEntry {
    #[serde(default)]
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) issue: Option<RedmineTimeEntryIssue>,
    #[serde(default)]
    pub(crate) activity: Option<RedmineTimeEntryActivity>,
    #[serde(default)]
    pub(crate) hours: Option<f64>,
    #[serde(default)]
    pub(crate) comments: Option<String>,
    #[serde(default)]
    pub(crate) spent_on: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RedmineTimeEntryIssue {
    pub(crate) id: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RedmineTimeEntryCollection {
    #[serde(default)]
    pub(crate) time_entries: Vec<RedmineTimeEntry>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineTimeEntryResponse {
    #[serde(default)]
    pub(crate) time_entry: Option<RedmineTimeEntry>,
}

/// Native Redmine Time Entry request payload.
#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewTimeEntry<'a> {
    pub(crate) time_entry: RedmineNewTimeEntryFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewTimeEntryFields<'a> {
    pub(crate) issue_id: u64,
    pub(crate) hours: f64,
    pub(crate) spent_on: &'a str,
    pub(crate) activity_id: u64,
    pub(crate) comments: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewIssue<'a> {
    pub(crate) issue: RedmineNewIssueFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewIssueFields<'a> {
    pub(crate) project_id: Value,
    pub(crate) subject: &'a str,
    pub(crate) description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tracker_id: Option<u64>,
    // Native Redmine planning fields. All are omitted when unset so the
    // legacy create payload stays byte-identical for existing callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_issue_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fixed_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) due_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) estimated_hours: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) done_ratio: Option<u64>,
}

/// Native Redmine issue planning fields shared by create and update
/// payloads. Every field is optional and omitted from JSON when unset so
/// callers that do not use planning flags keep the exact legacy payload.
#[derive(Clone, Debug, Default)]
pub(crate) struct IssuePlanning {
    pub(crate) parent_issue_id: Option<u64>,
    pub(crate) fixed_version_id: Option<u64>,
    pub(crate) start_date: Option<String>,
    pub(crate) due_date: Option<String>,
    pub(crate) estimated_hours: Option<f64>,
    pub(crate) done_ratio: Option<u64>,
}

impl IssuePlanning {
    /// True when no planning field is set; callers can keep using the
    /// plain provider paths that never grew planning fields.
    pub(crate) fn is_empty(&self) -> bool {
        self.parent_issue_id.is_none()
            && self.fixed_version_id.is_none()
            && self.start_date.is_none()
            && self.due_date.is_none()
            && self.estimated_hours.is_none()
            && self.done_ratio.is_none()
    }
}

impl<'a> RedmineNewIssue<'a> {
    pub(crate) fn new(project_id: &'a str, subject: &'a str, description: &'a str) -> Self {
        let project_id = project_id
            .parse::<u64>()
            .map_or_else(|_| Value::String(project_id.to_owned()), Value::from);
        Self {
            issue: RedmineNewIssueFields {
                project_id,
                subject,
                description,
                tracker_id: None,
                parent_issue_id: None,
                fixed_version_id: None,
                start_date: None,
                due_date: None,
                estimated_hours: None,
                done_ratio: None,
            },
        }
    }

    /// Attach an explicit tracker (for example Bug or Feature) resolved
    /// through `/trackers.json`; omitted for callers that do not select one.
    pub(crate) fn with_tracker(mut self, tracker_id: u64) -> Self {
        self.issue.tracker_id = Some(tracker_id);
        self
    }

    pub(crate) fn with_tracker_option(self, tracker_id: Option<u64>) -> Self {
        match tracker_id {
            Some(tracker_id) => self.with_tracker(tracker_id),
            None => self,
        }
    }

    /// Attach native planning fields (parent, version, dates, estimates).
    /// Fields absent from `planning` stay out of the serialized payload.
    pub(crate) fn with_planning(mut self, planning: &IssuePlanning) -> Self {
        let target = &mut self.issue;
        target.parent_issue_id = planning.parent_issue_id;
        target.fixed_version_id = planning.fixed_version_id;
        target.start_date = planning.start_date.clone();
        target.due_date = planning.due_date.clone();
        target.estimated_hours = planning.estimated_hours;
        target.done_ratio = planning.done_ratio;
        self
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewProject<'a> {
    pub(crate) project: RedmineNewProjectFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewProjectFields<'a> {
    pub(crate) name: &'a str,
    pub(crate) identifier: &'a str,
    pub(crate) is_public: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<&'a str>,
    /// Project modules enabled at creation. Includes the `repository`
    /// module so the bootstrap-registered Git repository attaches without
    /// a separate `PUT /projects/:id.json` call. Serialized only when set
    /// to preserve the existing request shape for callers that do not opt
    /// into module enablement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enabled_modules: Option<Vec<RedmineEnabledModule<'a>>>,
}

impl<'a> RedmineNewProject<'a> {
    pub(crate) fn new(name: &'a str, identifier: &'a str, description: Option<&'a str>) -> Self {
        Self {
            project: RedmineNewProjectFields {
                name,
                identifier,
                is_public: false,
                description,
                enabled_modules: None,
            },
        }
    }

    pub(crate) fn with_repository_module(mut self) -> Self {
        self.project.enabled_modules = Some(vec![RedmineEnabledModule { name: "repository" }]);
        self
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineEnabledModule<'a> {
    pub(crate) name: &'a str,
}

/// Request payload for `POST /sys/redmine_git_mirror/projects/<id>/repository`.
///
/// The `redmine_git_mirror` plugin expects a flat `{ "url": ... }` body and
/// does not wrap the value in `repository[...]` like core Redmine does.
#[derive(Debug, Serialize)]
pub(crate) struct RedmineGitMirrorRequest<'a> {
    pub(crate) url: &'a str,
}

impl<'a> RedmineGitMirrorRequest<'a> {
    pub(crate) fn new(url: &'a str) -> Self {
        Self { url }
    }
}

/// Response payload returned by the plugin on both `POST .../repository` and
/// `GET .../repository/<identifier>`. The plugin returns this body with
/// `202 Accepted` for a freshly queued mirror and with `200 OK` for an
/// existing mirror so the client can render the same fields either way.
#[derive(Debug, Deserialize)]
pub(crate) struct RedmineGitMirrorResponse {
    pub(crate) id: u64,
    pub(crate) project_id: u64,
    pub(crate) identifier: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) remote_url: Option<String>,
    #[serde(default)]
    pub(crate) local_path: Option<String>,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

/// Public outcome of one bootstrap's mirror plugin interaction, suitable for
/// inclusion in bootstrap JSON output. `status` is normalised to one of
/// `pending`, `cloning`, `ready`, `failed`, or `existing` (when the
/// bootstrap path only inspects the GET result and does not POST).
#[derive(Debug, Clone)]
pub struct RedmineGitMirrorOutcome {
    pub id: u64,
    pub project_id: u64,
    pub identifier: String,
    pub status: String,
    pub remote_url: String,
    pub local_path: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineUpdateIssue<'a> {
    pub(crate) issue: RedmineUpdateIssueFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineUpdateIssueFields<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tracker_id: Option<u64>,
    // Native planning fields, omitted when unset so the legacy update
    // payload stays byte-identical for existing callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_issue_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fixed_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) due_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) estimated_hours: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) done_ratio: Option<u64>,
}

impl<'a> RedmineUpdateIssueFields<'a> {
    fn empty(
        description: Option<&'a str>,
        status_id: Option<u64>,
        tracker_id: Option<u64>,
    ) -> Self {
        Self {
            description,
            status_id,
            tracker_id,
            parent_issue_id: None,
            fixed_version_id: None,
            start_date: None,
            due_date: None,
            estimated_hours: None,
            done_ratio: None,
        }
    }
}

impl<'a> RedmineUpdateIssue<'a> {
    pub(crate) fn description(description: &'a str) -> Self {
        Self {
            issue: RedmineUpdateIssueFields::empty(Some(description), None, None),
        }
    }

    /// Update the body and optionally re-target the tracker in one PUT so
    /// `issue update-body --tracker ...` stays a single atomic request.
    pub(crate) fn description_with_tracker(description: &'a str, tracker_id: u64) -> Self {
        Self {
            issue: RedmineUpdateIssueFields::empty(Some(description), None, Some(tracker_id)),
        }
    }

    pub(crate) fn status(status_id: u64) -> Self {
        Self {
            issue: RedmineUpdateIssueFields::empty(None, Some(status_id), None),
        }
    }

    /// Attach native planning fields (parent, version, dates, estimates).
    /// Fields absent from `planning` stay out of the serialized payload.
    pub(crate) fn with_planning(mut self, planning: &IssuePlanning) -> Self {
        self.issue.parent_issue_id = planning.parent_issue_id;
        self.issue.fixed_version_id = planning.fixed_version_id;
        self.issue.start_date = planning.start_date.clone();
        self.issue.due_date = planning.due_date.clone();
        self.issue.estimated_hours = planning.estimated_hours;
        self.issue.done_ratio = planning.done_ratio;
        self
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNotes<'a> {
    pub(crate) issue: RedmineNotesFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNotesFields<'a> {
    pub(crate) notes: &'a str,
}

impl RedmineIssue {
    pub(crate) fn into_summary(self, html_url: String) -> IssueSummary {
        let state = self.state();
        IssueSummary {
            id: self.id,
            number: self.id,
            title: self.subject,
            body: self.description,
            state,
            html_url: Some(html_url),
        }
    }

    pub(crate) fn matches_state(&self, state: &str) -> bool {
        match state {
            "all" => true,
            "open" => !self.status_is_closed(),
            "closed" => self.status_is_closed(),
            _ => false,
        }
    }

    pub(crate) fn state(&self) -> String {
        self.status
            .as_ref()
            .map(|status| {
                status.is_closed.map_or_else(
                    || status.name.clone(),
                    |closed| {
                        if closed {
                            "closed".to_owned()
                        } else {
                            "open".to_owned()
                        }
                    },
                )
            })
            .unwrap_or_else(|| "unknown".to_owned())
    }

    pub(crate) fn find_journal(&self, body: &str, marker: &str) -> Option<&RedmineJournal> {
        self.journals
            .iter()
            .rev()
            .find(|journal| journal.notes == body || journal.notes.contains(marker))
    }

    fn status_is_closed(&self) -> bool {
        self.status.as_ref().is_some_and(|status| {
            status
                .is_closed
                .unwrap_or_else(|| status.name.to_ascii_lowercase().contains("closed"))
        })
    }
}

impl RedmineIssueCollection {
    pub(crate) fn signature(&self) -> String {
        self.issues
            .iter()
            .map(|issue| issue.id.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl RedmineJournal {
    /// Render a journal as a comment whose URL anchors the exact note so
    /// audit references land on `#note-<id>` instead of the issue top.
    pub(crate) fn to_comment(
        &self,
        issue_url: &str,
        marker: Option<&str>,
        include_body: bool,
    ) -> CommentOutput {
        CommentOutput {
            id: self.id,
            html_url: Some(format!("{issue_url}#note-{}", self.id)),
            marker: marker
                .map(str::to_owned)
                .or_else(|| marker_from_notes(&self.notes)),
            body: include_body.then(|| self.notes.clone()),
        }
    }
}

fn marker_from_notes(notes: &str) -> Option<String> {
    let start = notes.find("<!--")?;
    let end = notes[start..].find("-->")? + start + 3;
    Some(notes[start..end].to_owned())
}

/// Redmine issue relation types used by this workflow.
///
/// Redmine stores a relation in one direction (`issue_id` -> `issue_to_id`)
/// with a stored type, but every pair also has an inverse name: `blocks` is
/// seen as `blocked` from the target issue's perspective, and `precedes` as
/// `follows`. `relates` is symmetric. Only the canonical names are accepted
/// as CLI input; the inverse names are only decoded from server responses so
/// list output can render each relation from the queried issue's viewpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RedmineRelationType {
    Blocks,
    Precedes,
    Relates,
    Blocked,
    Follows,
}

impl RedmineRelationType {
    /// Strict parse of the CLI-facing canonical names. Inverse names are
    /// rejected so callers can never create a relation whose direction
    /// contradicts its label.
    pub(crate) fn parse_input(value: &str) -> Result<Self, ForgejoError> {
        match value {
            "blocks" => Ok(Self::Blocks),
            "precedes" => Ok(Self::Precedes),
            "relates" => Ok(Self::Relates),
            other => Err(ForgejoError::config(format!(
                "Redmine relation type must be blocks, precedes, or relates (got '{other}')"
            ))),
        }
    }

    /// Strict decode of a server-side relation type, including inverse
    /// names Redmine reports for relations stored from the opposite side.
    pub(crate) fn parse(value: &str) -> Result<Self, ForgejoError> {
        match value {
            "blocks" => Ok(Self::Blocks),
            "precedes" => Ok(Self::Precedes),
            "relates" => Ok(Self::Relates),
            "blocked" => Ok(Self::Blocked),
            "follows" => Ok(Self::Follows),
            other => Err(ForgejoError::config(format!(
                "unknown Redmine relation type '{other}'"
            ))),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::Precedes => "precedes",
            Self::Relates => "relates",
            Self::Blocked => "blocked",
            Self::Follows => "follows",
        }
    }

    /// The relation as seen from the opposite issue. Redmine records a pair
    /// once (from `issue_id` to `issue_to_id`); `blocks` reads as `blocked`
    /// and `precedes` reads as `follows` from the target, while `relates` is
    /// symmetric. `relation list` renders each relation from the queried
    /// issue's viewpoint using exactly this mapping.
    pub(crate) const fn inverse(self) -> Self {
        match self {
            Self::Blocks => Self::Blocked,
            Self::Blocked => Self::Blocks,
            Self::Precedes => Self::Follows,
            Self::Follows => Self::Precedes,
            Self::Relates => Self::Relates,
        }
    }
}

/// A single Redmine issue relation as returned by
/// `/issues/:id/relations.json`. `relation_type` is the raw server value
/// (one of `blocks`, `precedes`, `relates`, `blocked`, `follows`); list
/// output re-derives the viewpoint-resolved name.
#[derive(Debug, Deserialize)]
pub(crate) struct RedmineRelation {
    pub(crate) id: u64,
    pub(crate) issue_id: u64,
    pub(crate) issue_to_id: u64,
    pub(crate) relation_type: String,
    #[serde(default)]
    pub(crate) delay: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineRelationCollection {
    #[serde(default)]
    pub(crate) relations: Vec<RedmineRelation>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineRelationResponse {
    pub(crate) relation: RedmineRelation,
}

/// Request body for `POST /issues/:id/relations.json`. `relation_type` is one
/// of the canonical CLI names (`blocks`, `precedes`, `relates`); `delay` is
/// only ever set for `precedes` and is omitted otherwise so the payload stays
/// minimal.
#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewRelation<'a> {
    pub(crate) relation: RedmineNewRelationFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewRelationFields<'a> {
    pub(crate) issue_to_id: u64,
    pub(crate) relation_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) delay: Option<u64>,
}

impl<'a> RedmineNewRelation<'a> {
    pub(crate) fn new(issue_to_id: u64, relation_type: &'a str, delay: Option<u64>) -> Self {
        Self {
            relation: RedmineNewRelationFields {
                issue_to_id,
                relation_type,
                delay,
            },
        }
    }
}

/// Public, serializable output for one issue relation. `relation_type` is the
/// name resolved from the queried issue's viewpoint (so a `blocks` stored
/// from the source's side shows as `blocked` when listing the target), and
/// `issue_id` / `issue_to_id` carry the raw endpoints so callers can see the
/// direction.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RelationSummary {
    pub id: u64,
    pub relation_type: String,
    pub issue_id: u64,
    pub issue_to_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<u64>,
}

impl RedmineRelation {
    /// Render this relation from the viewpoint of `queried_issue_id`. When the
    /// queried issue is the relation's `issue_to_id`, the stored type is
    /// inverted (`blocks` -> `blocked`, `precedes` -> `follows`) so the
    /// output matches what a user inspecting that issue expects. Unknown
    /// server types are surfaced verbatim rather than dropped.
    pub(crate) fn into_summary(self, queried_issue_id: u64) -> RelationSummary {
        let relation_type = match RedmineRelationType::parse(&self.relation_type) {
            Ok(parsed) if self.issue_id == queried_issue_id => parsed.as_str().to_owned(),
            Ok(parsed) => parsed.inverse().as_str().to_owned(),
            Err(_) => self.relation_type.clone(),
        };
        RelationSummary {
            id: self.id,
            relation_type,
            issue_id: self.issue_id,
            issue_to_id: self.issue_to_id,
            delay: self.delay,
        }
    }
}
