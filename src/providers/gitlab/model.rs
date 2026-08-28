//! GitLab REST v4 models for issue, note, and label payloads plus the
//! typed helpers that translate workflow status and tracker names into
//! GitLab label conventions.
//!
//! The structures are deliberately narrow: only the fields the
//! orchestrator CLI actually consumes (id/iid/title/description/state/
//! web_url for issues; id/body for notes; name/color for labels) are
//! decoded. Adding fields is cheap; misinterpreting an unknown GitLab
//! payload field as an authoritative state value is not, so the
//! decoders stay minimal.

use crate::providers::api::ForgejoError;
use serde::{Deserialize, Serialize};

/// Workflow labels that map orchestrator workflow statuses to GitLab
/// project labels. These are managed by this CLI: an issue update
/// removes any prior `workflow::*` labels and applies exactly one of
/// these labels for the requested status. `workflow::closed` is the
/// only label that is paired with GitLab's native close transition.
pub(crate) const WORKFLOW_LABEL_NEW: &str = "workflow::new";
pub(crate) const WORKFLOW_LABEL_IN_PROGRESS: &str = "workflow::in-progress";
pub(crate) const WORKFLOW_LABEL_IN_REVIEW: &str = "workflow::in-review";
pub(crate) const WORKFLOW_LABEL_CHANGES_REQUESTED: &str = "workflow::changes-requested";
pub(crate) const WORKFLOW_LABEL_BLOCKED: &str = "workflow::blocked";
pub(crate) const WORKFLOW_LABEL_RESOLVED: &str = "workflow::resolved";
pub(crate) const WORKFLOW_LABEL_CLOSED: &str = "workflow::closed";
pub(crate) const WORKFLOW_LABEL_CANCELLED: &str = "workflow::cancelled";

/// Every workflow label this CLI recognises, in a stable iteration
/// order. Used by the workflow updater to know which labels are safe
/// to remove from an issue.
pub(crate) const WORKFLOW_LABELS: &[&str] = &[
    WORKFLOW_LABEL_NEW,
    WORKFLOW_LABEL_IN_PROGRESS,
    WORKFLOW_LABEL_IN_REVIEW,
    WORKFLOW_LABEL_CHANGES_REQUESTED,
    WORKFLOW_LABEL_BLOCKED,
    WORKFLOW_LABEL_RESOLVED,
    WORKFLOW_LABEL_CLOSED,
    WORKFLOW_LABEL_CANCELLED,
];

/// Project labels used to encode the Redmine-style Bug / Feature
/// trackers. GitLab has no first-class tracker concept; we use labels
/// so a single create or update request can carry the tracker
/// alongside other fields without a separate API call.
pub(crate) const TRACKER_LABEL_BUG: &str = "type::bug";
pub(crate) const TRACKER_LABEL_FEATURE: &str = "type::feature";

/// Resolve a tracker name to its GitLab label.
///
/// Accepts the case-sensitive names "Bug" and "Feature" (matching the
/// canonical Redmine spelling) and the same names with arbitrary ASCII
/// casing, so a Redmine caller that already learned the
/// `RedmineProvider::select_tracker` rules can pass through. Numeric
/// ids are rejected because the GitLab label convention is name-based
/// and a project can have multiple `type::*` labels; mapping an id to
/// a label would require a separate metadata round trip that Phase 2
/// explicitly defers.
pub(crate) fn tracker_label_from_name(value: &str) -> Result<&'static str, ForgejoError> {
    if value.eq_ignore_ascii_case("Bug") {
        Ok(TRACKER_LABEL_BUG)
    } else if value.eq_ignore_ascii_case("Feature") {
        Ok(TRACKER_LABEL_FEATURE)
    } else {
        Err(ForgejoError::config(format!(
            "GitLab tracker name '{value}' must be Bug or Feature"
        )))
    }
}

/// Resolve a GitLab label back to its canonical tracker name. Used
/// by managed-label validation and future read paths that need the inverse mapping.
pub(crate) fn tracker_name_from_label(label: &str) -> Option<&'static str> {
    match label {
        TRACKER_LABEL_BUG => Some("Bug"),
        TRACKER_LABEL_FEATURE => Some("Feature"),
        _ => None,
    }
}

/// `Err(ForgejoError::config(...))` for unknown statuses so a typo
/// never lands as a silent no-op update.
///
/// Names are case-insensitive so a caller that lower-cases the
/// orchestrator's status value still resolves; the label returned is
/// the canonical lowercase form GitLab receives.
pub(crate) fn workflow_label_from_status(status: &str) -> Result<&'static str, ForgejoError> {
    let normalised = status.trim();
    if normalised.eq_ignore_ascii_case("New") {
        Ok(WORKFLOW_LABEL_NEW)
    } else if normalised.eq_ignore_ascii_case("InProgress")
        || normalised.eq_ignore_ascii_case("In Progress")
    {
        Ok(WORKFLOW_LABEL_IN_PROGRESS)
    } else if normalised.eq_ignore_ascii_case("InReview")
        || normalised.eq_ignore_ascii_case("In Review")
    {
        Ok(WORKFLOW_LABEL_IN_REVIEW)
    } else if normalised.eq_ignore_ascii_case("ChangesRequested")
        || normalised.eq_ignore_ascii_case("Changes Requested")
    {
        Ok(WORKFLOW_LABEL_CHANGES_REQUESTED)
    } else if normalised.eq_ignore_ascii_case("Blocked") {
        Ok(WORKFLOW_LABEL_BLOCKED)
    } else if normalised.eq_ignore_ascii_case("Resolved") {
        Ok(WORKFLOW_LABEL_RESOLVED)
    } else if normalised.eq_ignore_ascii_case("Closed") {
        Ok(WORKFLOW_LABEL_CLOSED)
    } else if normalised.eq_ignore_ascii_case("Cancelled")
        || normalised.eq_ignore_ascii_case("Canceled")
    {
        Ok(WORKFLOW_LABEL_CANCELLED)
    } else {
        Err(ForgejoError::config(format!(
            "GitLab workflow status '{status}' is not recognised; expected \
             New, InProgress, InReview, ChangesRequested, Blocked, Resolved, \
             Closed, or Cancelled"
        )))
    }
}

/// Map a GitLab issue `state` value to the orchestrator's shared
/// `open` / `closed` vocabulary. GitLab only reports `opened` and
/// `closed` at the issue level; the workflow label handles the
/// finer-grained distinction.
pub(crate) fn state_from_gitlab(state: &str) -> &'static str {
    if state.eq_ignore_ascii_case("closed") {
        "closed"
    } else {
        "open"
    }
}

/// Map the orchestrator's `open` / `closed` / `all` state selector to
/// the GitLab issue state filter. `all` is signalled via `None` so the
/// caller knows not to send `state=opened` or `state=closed`.
pub(crate) fn state_query_filter(state: &str) -> Result<Option<&'static str>, ForgejoError> {
    match state {
        "open" => Ok(Some("opened")),
        "closed" => Ok(Some("closed")),
        "all" => Ok(None),
        other => Err(ForgejoError::config(format!(
            "issue state '{other}' must be open, closed, or all"
        ))),
    }
}

/// JSON payload returned by `GET /projects/:id/issues/:iid`. The
/// `iid` field is the project-scoped issue number that the
/// orchestrator surfaces as `IssueSummary::number`; the global `id`
/// is recorded but the CLI only uses it for diagnostic logging in the
/// audit comment shape.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssue {
    pub id: u64,
    pub iid: u64,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub web_url: Option<String>,
}

/// Request payload for `POST /projects/:id/issues`.
#[derive(Debug, Serialize)]
pub(crate) struct NewIssue<'a> {
    pub title: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub description: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// Request payload for `PUT /projects/:id/issues/:iid`. Every field
/// is optional so the caller can target a single aspect of the issue
/// (body, state, labels) without accidentally clearing the others.
#[derive(Debug, Default, Serialize)]
pub(crate) struct UpdateIssue<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_event: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add_labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove_labels: Vec<String>,
}

/// JSON payload returned by `POST /projects/:id/issues/:iid/notes`
/// and `GET /projects/:id/issues/:iid/notes/:note_id`.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiNote {
    pub id: u64,
    pub body: String,
}

/// Request payload for `POST /projects/:id/issues/:iid/notes`.
#[derive(Debug, Serialize)]
pub(crate) struct NewNote<'a> {
    pub body: &'a str,
}

/// JSON payload returned by GitLab label endpoints.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ApiLabel {
    pub name: String,
}

/// Request payload for `POST /projects/:id/labels`.
#[derive(Debug, Serialize)]
pub(crate) struct NewLabel<'a> {
    pub name: &'a str,
    pub color: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
}

/// JSON payload returned by `POST /projects` and `GET /projects/:id`.
///
/// GitLab echoes the namespace as a nested object (`{ "id": …, "path": …,
/// "full_path": …, "kind": "user"|"group" }`); only `path` and `full_path`
/// matter for the orchestrator's repository summary. `name` and `path` are
/// kept separate because GitLab uses `path` as the URL slug.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiProject {
    pub path: String,
    #[serde(default)]
    pub path_with_namespace: Option<String>,
    #[serde(default)]
    pub web_url: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub namespace: Option<ApiProjectNamespace>,
    #[serde(default)]
    pub http_url_to_repo: Option<String>,
    #[serde(default)]
    pub ssh_url_to_repo: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiProjectNamespace {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub full_path: Option<String>,
}

/// JSON payload returned by `GET /namespaces?search=…`. The
/// orchestrator uses this endpoint to resolve an OWNER path to a
/// numeric `namespace_id` so a `repo create OWNER/REPO` call lands
/// in the right group rather than the authenticated user's personal
/// namespace. `kind` distinguishes `user` from `group` namespaces so
/// the resolver can flag ambiguous matches and prefer group ids
/// when both share the same path.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiNamespace {
    pub id: u64,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// Request payload for `POST /projects`. All optional fields are
/// skipped during serialization so a private-only call (the only
/// path the orchestrator exercises today) stays minimal.
#[derive(Debug, Serialize)]
pub(crate) struct NewProject<'a> {
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<&'a str>,
    /// `namespace_id` is preferred when an explicit group or user
    /// namespace id was supplied; it is mutually exclusive with the
    /// `namespace` path. The provider picks whichever the caller
    /// resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<&'a str>,
    /// GitLab accepts `private`, `internal`, and `public`. The
    /// orchestrator's repo CLI is private-only, so `visibility` is
    /// always set to `private` when the caller marks the project
    /// private. The field is included even when the value is `private`
    /// because GitLab's default for new projects is `private` only
    /// when the parent namespace forces it; without `visibility` an
    /// explicit request could land in a more permissive bucket by
    /// accident.
    pub visibility: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub description: &'a str,
    /// Maps the Forgejo-style `auto_init` flag onto GitLab's
    /// `initialize_with_readme`. The orchestrator uses `initialize_with_readme`
    /// because it is the only documented way to force a `README.md`
    /// commit on creation.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub initialize_with_readme: bool,
}

/// JSON payload returned by `GET /projects/:id/pipelines` and the
/// single-pipeline endpoint.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiPipeline {
    pub id: u64,
    #[serde(default)]
    pub iid: u64,
    #[serde(default)]
    pub status: String,
    /// GitLab returns the branch / tag name as the JSON key `ref`.
    /// The orchestrator uses `ref_name` internally to avoid the
    /// Rust reserved word.
    #[serde(default, rename = "ref")]
    pub ref_name: Option<String>,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub before_sha: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub web_url: Option<String>,
}

/// JSON payload returned by `GET /projects/:id/pipelines/:pipeline_id/jobs`.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiJob {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub pipeline: Option<ApiJobPipelineRef>,
    #[serde(default)]
    pub queued_duration: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiJobPipelineRef {
    #[serde(default)]
    pub id: Option<u64>,
}

/// Map a GitLab pipeline `status` value to the orchestrator's shared
/// `CiRunSummary::status` vocabulary. The Forgejo mapping is the
/// reference; we keep the same lowercase string here so downstream
/// consumers can compare them without special-casing GitLab.
///
/// GitLab exposes `created`, `waiting_for_resource`, `preparing`,
/// `pending`, `running`, `success`, `failed`, `canceled`, `skipped`,
/// `manual`, and `scheduled`. The shared vocabulary keeps
/// `running` / `pending` / `success` / `failure` semantics and adds
/// `canceled`, `skipped`, and `manual` because those GitLab states
/// do not map cleanly to either `running` or `failure`.
pub(crate) fn pipeline_status_from_gitlab(status: &str) -> String {
    let normalised = status.to_ascii_lowercase();
    match normalised.as_str() {
        "created" | "waiting_for_resource" | "preparing" | "pending" | "scheduled" => {
            "pending".to_owned()
        }
        "running" => "running".to_owned(),
        "success" => "success".to_owned(),
        "failed" => "failure".to_owned(),
        "canceled" | "cancelled" => "cancelled".to_owned(),
        "skipped" => "skipped".to_owned(),
        "manual" => "manual".to_owned(),
        // Unknown future values: keep them visible rather than silently
        // remapping to "unknown".
        other => other.to_owned(),
    }
}

/// Resolve the optional `conclusion` field that GitLab exposes for
/// finished pipelines / jobs. The shared model uses `None` while the
/// pipeline is still running and the literal conclusion string once
/// the pipeline finishes. GitLab returns the same string as `status`
/// for finished pipelines, so the status is the authoritative source.
pub(crate) fn pipeline_conclusion_from_gitlab(
    status: &str,
    conclusion: Option<&str>,
) -> Option<String> {
    let normalised = status.to_ascii_lowercase();
    match normalised.as_str() {
        "success" | "failed" | "canceled" | "cancelled" | "skipped" => Some(normalised),
        "running"
        | "pending"
        | "created"
        | "waiting_for_resource"
        | "preparing"
        | "scheduled"
        | "manual" => None,
        _ => conclusion
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.to_ascii_lowercase()),
    }
}

/// JSON error payload returned by GitLab for non-2xx responses.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiError {
    #[serde(default)]
    pub message: Option<String>,
    /// Some endpoints return `{ "error": "..." }` instead of a
    /// nested object; capture that too so the rendered error stays
    /// informative.
    #[serde(default)]
    pub error: Option<String>,
    /// GitLab occasionally wraps the human-readable error in an
    /// array (for example `{ "message": { "xxx": ["..."] } }`); the
    /// structured variant catches that case.
    #[serde(default)]
    pub error_description: Option<String>,
}

/// Request payload for `POST /projects/:id/issues/:iid/add_spent_time`.
/// GitLab's documented `summary` is a free-text label the user can use
/// to disambiguate individual entries; we use it as the durable
/// run-marker anchor so retries can be reconciled without inventing a
/// fake remote id.
#[derive(Debug, Serialize)]
pub(crate) struct NewSpentTime<'a> {
    pub duration: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<&'a str>,
}

/// Request payload for `POST /projects/:id/issues/:iid/time_estimate`.
/// GitLab stores a numeric second-precision estimate; the human-format
/// `duration` is a string so callers do not have to multiply hours by
/// 3600 themselves.
#[derive(Debug, Serialize)]
pub(crate) struct NewTimeEstimate<'a> {
    pub duration: &'a str,
}

/// Response payload returned by `GET /projects/:id/issues/:iid/links`
/// and `POST /projects/:id/issues/:iid/links`.
///
/// GitLab 19.x returns two distinct shapes for the same logical
/// resource:
///
/// * `POST /projects/:id/issues/:iid/links` returns
///   `{ "id", "source_issue", "target_issue", "link_type", ... }`.
///   The link id is the top-level `id`; the source/target endpoints
///   carry the `id`, `iid`, and `project_id` of each end.
/// * `GET /projects/:id/issues/:iid/links` returns an array where
///   each element is the **target issue object** plus the link id
///   (`issue_link_id`) and `link_type` attached at the top level:
///   `{ "id", "iid", "project_id", "issue_link_id", "link_type",
///   "title", "state", ... }`.
///
/// Earlier contract fixtures (and the GitLab REST v4 docs for
/// older releases) wrapped the target endpoint under `issue`; the
/// decoder keeps that nested field so the list path stays
/// compatible with the existing fixtures while the live GET
/// response shape is fully supported.
///
/// All id-bearing fields are optional so a partial payload (for
/// example one missing `link_create_user_id`) still decodes
/// instead of producing a `missing field` decode error.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssueLink {
    /// Link id returned by `POST /links` on the live instance.
    #[serde(default)]
    pub id: Option<u64>,
    /// Link id attached to the target issue object on
    /// `GET /links` (live shape and the earlier contract
    /// fixtures). The read path prefers this field so the legacy
    /// and live response shapes both decode to the same link id.
    #[serde(default)]
    pub issue_link_id: Option<u64>,
    /// GitLab link-type string (`relates_to`, `blocks`,
    /// `is_blocked_by`). Empty when the server omits the field;
    /// [`ApiIssueLink::into_summary`] surfaces the raw value so an
    /// operator can spot a regression rather than seeing a silent
    /// `relates` default.
    #[serde(default)]
    pub link_type: String,
    /// Target endpoint, only present in the live POST response.
    #[serde(default)]
    pub target_issue: Option<ApiIssueLinkEndpoint>,
    /// Target endpoint, used by the existing contract fixtures and
    /// by older GitLab releases that wrap the target issue inside
    /// a nested `issue` object.
    #[serde(default)]
    pub issue: Option<ApiIssueLinkIssue>,
    /// Target issue iid at the top level of the live GET
    /// response. Mirrors `ApiIssueLinkIssue::iid` and is kept as a
    /// separate field so the flat live shape decodes without
    /// forcing the caller to inspect the nested object.
    #[serde(default)]
    pub iid: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssueLinkEndpoint {
    pub iid: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssueLinkIssue {
    pub iid: u64,
}

/// Whether `relation_type` is accepted by `POST /links` on the live
/// `https://gitlab.example.com/19.2` instance.
///
/// The instance accepts `relates_to` and rejects `blocks` /
/// `is_blocked_by` with `link_type does not have a valid value`
/// even when the request is sent with the documented query
/// parameters. The decision is made locally (no network probe) so
/// the unsupported directions fail with a structured
/// [`crate::providers::api::ForgejoError::NotSupported`] error
/// before any HTTP traffic. The read path still maps every
/// server-returned link type (`blocks`, `is_blocked_by`) so the
/// list output reflects whatever the server already recorded.
pub(crate) fn gitlab_create_supports_relation_type(
    relation_type: crate::providers::redmine::model::RedmineRelationType,
) -> bool {
    use crate::providers::redmine::model::RedmineRelationType;
    matches!(relation_type, RedmineRelationType::Relates)
}

/// Outcome of a single `add_spent_time` or `set_time_estimate`
/// request. GitLab REST v4 returns the updated issue totals so the
/// caller can render the new state without a follow-up GET, but it
/// does NOT return a per-entry identifier that the orchestrator
/// could persist as a remote id and reconcile against on retry; the
/// local SQLite ledger is therefore the sole idempotency marker for
/// the timer path, and `time_entry_id` is intentionally left `None`
/// after a successful GitLab projection.
///
/// Three response shapes are accepted by the decoder:
///
/// 1. The flat documented shape returned by older GitLab releases:
///    `{ "seconds": …, "human_readable": …, "total_seconds": …,
///    "total_human_readable": … }`.
/// 2. The issue-shaped body returned by GitLab 19.x on the live
///    `https://gitlab.example.com/19.2` instance for some
///    endpoints: the response echoes the full `ApiIssue` payload
///    with the running totals wrapped under a nested `time_stats`
///    block (`{ "time_stats": { "total_time_spent",
///    "time_estimate", "human_total_time_spent",
///    "human_time_estimate", ... } }`).
/// 3. The top-level time-stats body returned by GitLab 19.x for
///    `POST /projects/:id/issues/:iid/add_spent_time` and
///    `POST /projects/:id/issues/:iid/time_estimate`: the response
///    is a flat object whose top-level fields are the time-stats
///    totals (`{ "time_estimate", "total_time_spent",
///    "human_time_estimate", "human_total_time_spent" }`). This
///    shape was confirmed live against project 3 issue 5 on the
///    `https://gitlab.example.com/19.2` instance.
///
/// Every variant is parsed without inventing a remote id, and the
/// documented flat serialised projection stays a 4-key object via
/// `skip_serializing` (no condition) on every wire-compatibility
/// field plus `skip_serializing_if = "Option::is_none"` on the
/// original 4 flat keys; the round-trip contract test in
/// `gitlab_contract_tests.rs` keeps pinning the documented shape.
/// Use [`Self::is_confirmed`] to decide whether the response
/// confirms a successful write regardless of the shape GitLab
/// happened to return.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ApiSpentTimeSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_readable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_human_readable: Option<String>,
    #[serde(default, skip_serializing)]
    pub time_estimate: Option<i64>,
    #[serde(default, skip_serializing)]
    pub total_time_spent: Option<i64>,
    #[serde(default, skip_serializing)]
    pub human_time_estimate: Option<String>,
    #[serde(default, skip_serializing)]
    pub human_total_time_spent: Option<String>,
    /// Issue-shaped body returned by GitLab 19.x on the live
    /// `https://gitlab.example.com/19.2` instance. The flat
    /// fields above stay `None` when GitLab returns the wrapped
    /// shape; callers MUST go through [`Self::is_confirmed`] so the
    /// projection handles either form. The field is deserialised
    /// only; `skip_serializing` (no condition) keeps it out of the
    /// serialised projection so the documented 4-key round-trip
    /// contract stays stable regardless of the input shape.
    #[serde(default, skip_serializing)]
    pub time_stats: Option<ApiIssueTimeStats>,
}

/// Subset of the `time_stats` block carried by the GitLab issue
/// payload. Only the four fields the projection uses for confirmation
/// are decoded; any additional server-side fields are silently
/// ignored so a future GitLab release can extend the payload without
/// breaking the client. `Serialize` is derived so the parent
/// `ApiSpentTimeSummary` (which derives both `Deserialize` and
/// `Serialize` for round-trip contract coverage) keeps compiling;
/// the inner field is skipped during serialisation by the parent,
/// so `Serialize` is never observed in the wire format.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ApiIssueTimeStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_estimate: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_time_spent: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_time_estimate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_total_time_spent: Option<String>,
}

impl ApiSpentTimeSummary {
    /// True when the response confirms a successful spent-time or
    /// time-estimate write. Accepts every response shape the live
    /// GitLab 19.x instance has been observed to return:
    ///
    /// * The flat documented shape: `seconds` or `total_seconds`
    ///   populated.
    /// * The GitLab 19.x issue-shaped body: `time_stats` carries a
    ///   non-null `total_time_spent` (for `add_spent_time`) or
    ///   `time_estimate` (for `set_time_estimate`).
    /// * The top-level time-stats body: `total_time_spent`
    ///   populated (for `add_spent_time`) or `time_estimate`
    ///   populated (for `set_time_estimate`).
    ///
    /// A fully empty / unknown-shape response stays `false` so the
    /// retry path keeps its structured `unconfirmed` semantics for
    /// genuinely ambiguous results; failure and already-synced
    /// ordering are handled by the caller (see
    /// [`crate::time_tracking_cli::project_run_with_gitlab_provider`]).
    pub(crate) fn is_confirmed(&self) -> bool {
        self.seconds.is_some()
            || self.total_seconds.is_some()
            || self.time_estimate.is_some()
            || self.total_time_spent.is_some()
            || self
                .time_stats
                .as_ref()
                .and_then(|stats| stats.total_time_spent)
                .is_some()
            || self
                .time_stats
                .as_ref()
                .and_then(|stats| stats.time_estimate)
                .is_some()
            || self
                .human_time_estimate
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .human_total_time_spent
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .time_stats
                .as_ref()
                .and_then(|stats| stats.human_time_estimate.as_deref())
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .time_stats
                .as_ref()
                .and_then(|stats| stats.human_total_time_spent.as_deref())
                .is_some_and(|value| !value.trim().is_empty())
    }
}

/// Map the orchestrator's Redmine-style canonical relation name to
/// the GitLab `link_type` spelling. `relates` maps to `relates_to`,
/// `blocks` maps to `blocks`, and `Precedes` is rejected before the
/// mapping so the structured not-supported error surfaces from the CLI
/// layer instead of an HTTP 400.
///
/// `Blocked` is GitLab's `is_blocked_by`: when listing links the source
/// issue can carry an `is_blocked_by` link that records the inverse
/// direction. Direct CLI input never uses `Blocked`/`Follows`; the
/// mapping only matters when normalising server responses.
pub(crate) fn gitlab_link_type_from_relation_type(
    relation_type: crate::providers::redmine::model::RedmineRelationType,
) -> Result<&'static str, ForgejoError> {
    use crate::providers::redmine::model::RedmineRelationType;
    match relation_type {
        RedmineRelationType::Relates => Ok("relates_to"),
        RedmineRelationType::Blocks => Ok("blocks"),
        RedmineRelationType::Precedes => Err(ForgejoError::config(
            "GitLab issue links do not support --type precedes",
        )),
        // Inverse direction accepted only on the read path; calling
        // code never passes Blocked/Follows through the CLI parser.
        RedmineRelationType::Blocked | RedmineRelationType::Follows => Err(ForgejoError::config(
            "GitLab issue links accept only the forward canonical names blocks and relates",
        )),
    }
}

/// Map GitLab's `link_type` to the canonical CLI name. Used by
/// `relation list` so the wire format stays GitLab-shaped while the
/// CLI output matches Redmine's vocabulary.
pub(crate) fn gitlab_link_type_to_relation_type(
    link_type: &str,
) -> crate::providers::redmine::model::RedmineRelationType {
    use crate::providers::redmine::model::RedmineRelationType;
    match link_type {
        "relates_to" => RedmineRelationType::Relates,
        "blocks" => RedmineRelationType::Blocks,
        "is_blocked_by" => RedmineRelationType::Blocked,
        // Future GitLab additions are surfaced as the lowercased raw
        // string when emitted through `RelationSummary::relation_type`;
        // the typed enum only carries the known three. The mapper
        // falls back to Relates to keep the typed boundary total so
        // a `RedmineRelationType` value is always producible.
        _ => RedmineRelationType::Relates,
    }
}

/// Format a positive second count as a GitLab human duration string.
/// GitLab's documented format is the concatenation of any subset of
/// `Nd`, `Nh`, `Nm`, `Ns` (for example `1h30m`, `45m`, `2d4h`). The
/// helper emits only the non-zero parts and rounds sub-second values
/// up so every duration represents at least one second.
pub(crate) fn format_gitlab_duration(seconds: i64) -> String {
    let mut total = seconds;
    if total < 0 {
        return "0s".to_owned();
    }
    if total == 0 {
        return "1s".to_owned();
    }
    let mut parts: Vec<String> = Vec::new();
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let days = total / DAY;
    if days > 0 {
        parts.push(format!("{days}d"));
        total -= days * DAY;
    }
    let hours = total / HOUR;
    if hours > 0 {
        parts.push(format!("{hours}h"));
        total -= hours * HOUR;
    }
    let minutes = total / MINUTE;
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
        total -= minutes * MINUTE;
    }
    if total > 0 || parts.is_empty() {
        parts.push(format!("{total}s"));
    }
    parts.join("")
}

#[cfg(test)]
mod tests {
    use super::{
        WORKFLOW_LABELS, format_gitlab_duration, gitlab_link_type_from_relation_type,
        gitlab_link_type_to_relation_type, state_from_gitlab, state_query_filter,
        tracker_label_from_name, tracker_name_from_label, workflow_label_from_status,
    };
    use crate::providers::api::ForgejoError;

    #[test]
    fn tracker_label_round_trip_for_bug_and_feature() {
        assert_eq!(tracker_label_from_name("Bug").unwrap(), "type::bug");
        assert_eq!(tracker_label_from_name("Feature").unwrap(), "type::feature");
        // Case-insensitive acceptance mirrors the Redmine selector.
        assert_eq!(tracker_label_from_name("bug").unwrap(), "type::bug");
        assert_eq!(tracker_label_from_name("FEATURE").unwrap(), "type::feature");
        assert_eq!(tracker_name_from_label("type::bug"), Some("Bug"));
        assert_eq!(tracker_name_from_label("type::feature"), Some("Feature"));
        assert_eq!(tracker_name_from_label("type::chore"), None);
    }

    #[test]
    fn tracker_label_rejects_other_values() {
        let error = tracker_label_from_name("Task").unwrap_err();
        match error {
            ForgejoError::Config(message) => {
                assert!(message.contains("GitLab tracker name 'Task'"));
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn tracker_label_rejects_numeric_ids() {
        // Numeric ids are explicitly unsupported in Phase 2: the
        // label-based convention is name-only.
        let error = tracker_label_from_name("2").unwrap_err();
        assert!(matches!(error, ForgejoError::Config(_)));
    }

    #[test]
    fn workflow_label_resolves_every_canonical_status() {
        let cases = [
            ("New", "workflow::new"),
            ("InProgress", "workflow::in-progress"),
            ("InProgress ", "workflow::in-progress"),
            ("InProgress\n", "workflow::in-progress"),
            ("inprogress", "workflow::in-progress"),
            ("InReview", "workflow::in-review"),
            ("ChangesRequested", "workflow::changes-requested"),
            ("Blocked", "workflow::blocked"),
            ("Resolved", "workflow::resolved"),
            ("Closed", "workflow::closed"),
            ("Cancelled", "workflow::cancelled"),
            ("Canceled", "workflow::cancelled"),
        ];
        for (input, expected) in cases {
            assert_eq!(workflow_label_from_status(input).unwrap(), expected);
        }
    }

    #[test]
    fn workflow_label_rejects_unknown_status() {
        let error = workflow_label_from_status("Reviewing").unwrap_err();
        match error {
            ForgejoError::Config(message) => assert!(message.contains("not recognised")),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn workflow_labels_list_covers_every_mapping() {
        // The mapping helper must reference exactly the labels that
        // are part of the managed list, otherwise a workflow update
        // would silently leave a stale label attached.
        for label in [
            "workflow::new",
            "workflow::in-progress",
            "workflow::in-review",
            "workflow::changes-requested",
            "workflow::blocked",
            "workflow::resolved",
            "workflow::closed",
            "workflow::cancelled",
        ] {
            assert!(
                WORKFLOW_LABELS.contains(&label),
                "{label} must be in WORKFLOW_LABABS",
            );
        }
        assert_eq!(WORKFLOW_LABELS.len(), 8);
    }

    #[test]
    fn state_query_filter_maps_open_closed_and_all() {
        assert_eq!(state_query_filter("open").unwrap(), Some("opened"));
        assert_eq!(state_query_filter("closed").unwrap(), Some("closed"));
        assert_eq!(state_query_filter("all").unwrap(), None);
        assert!(state_query_filter("bogus").is_err());
    }

    #[test]
    fn state_from_gitlab_uses_shared_open_closed_vocabulary() {
        assert_eq!(state_from_gitlab("opened"), "open");
        assert_eq!(state_from_gitlab("closed"), "closed");
        assert_eq!(state_from_gitlab("OPENED"), "open");
        assert_eq!(state_from_gitlab("Closed"), "closed");
        // Any other value (for example a future GitLab state) falls
        // back to the open bucket rather than silently dropping the
        // issue from search results.
        assert_eq!(state_from_gitlab("locked"), "open");
    }

    #[test]
    fn format_gitlab_duration_handles_every_part_with_zero_padding() {
        assert_eq!(format_gitlab_duration(0), "1s");
        assert_eq!(format_gitlab_duration(1), "1s");
        assert_eq!(format_gitlab_duration(60), "1m");
        assert_eq!(format_gitlab_duration(61), "1m1s");
        assert_eq!(format_gitlab_duration(3_600), "1h");
        assert_eq!(format_gitlab_duration(3_661), "1h1m1s");
        assert_eq!(format_gitlab_duration(5_400), "1h30m");
        assert_eq!(format_gitlab_duration(86_400), "1d");
        assert_eq!(format_gitlab_duration(86_400 + 5_400), "1d1h30m");
        assert_eq!(format_gitlab_duration(-1), "0s");
    }

    #[test]
    fn gitlab_link_type_maps_canonical_names_and_inverse() {
        use crate::providers::redmine::model::RedmineRelationType;
        assert_eq!(
            gitlab_link_type_from_relation_type(RedmineRelationType::Relates).unwrap(),
            "relates_to",
        );
        assert_eq!(
            gitlab_link_type_from_relation_type(RedmineRelationType::Blocks).unwrap(),
            "blocks",
        );
        let error = gitlab_link_type_from_relation_type(RedmineRelationType::Precedes).unwrap_err();
        assert!(
            error.json()["message"]
                .as_str()
                .unwrap_or_default()
                .contains("precedes")
        );
    }

    #[test]
    fn gitlab_link_type_decode_uses_canonical_inverse_mapping() {
        use crate::providers::redmine::model::RedmineRelationType;
        assert_eq!(
            gitlab_link_type_to_relation_type("relates_to"),
            RedmineRelationType::Relates,
        );
        assert_eq!(
            gitlab_link_type_to_relation_type("blocks"),
            RedmineRelationType::Blocks,
        );
        assert_eq!(
            gitlab_link_type_to_relation_type("is_blocked_by"),
            RedmineRelationType::Blocked,
        );
    }

    #[test]
    fn spent_time_summary_is_confirmed_accepts_flat_nested_and_top_level_shapes() {
        use super::ApiSpentTimeSummary;

        // Fully empty body (GitLab 204 or unknown-shape failure):
        // not confirmed; the projection must keep its unconfirmed
        // semantics so the retry path stays safe.
        let empty: ApiSpentTimeSummary = serde_json::from_str("{}").unwrap();
        assert!(!empty.is_confirmed());

        // Documented flat shape: confirmed via total_seconds.
        let flat: ApiSpentTimeSummary = serde_json::from_str(
            r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
        )
        .unwrap();
        assert!(flat.is_confirmed());

        // Live GitLab 19.x issue shape for spent time: confirmed via
        // time_stats.total_time_spent.
        let live_spent: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "id": 7,
                "iid": 2,
                "state": "opened",
                "time_stats": {
                    "time_estimate": 0,
                    "total_time_spent": 2,
                    "human_time_estimate": null,
                    "human_total_time_spent": "2s"
                }
            }"#,
        )
        .unwrap();
        assert!(live_spent.is_confirmed());
        let stats = live_spent
            .time_stats
            .as_ref()
            .expect("live shape must decode nested time_stats");
        assert_eq!(stats.total_time_spent, Some(2));
        assert_eq!(stats.human_total_time_spent.as_deref(), Some("2s"));
        assert_eq!(stats.time_estimate, Some(0));
        // Flat totals stay None because the live response carries
        // them only under time_stats; callers must not mistake a
        // nested response for the flat shape.
        assert!(live_spent.seconds.is_none());
        assert!(live_spent.total_seconds.is_none());
        // The top-level time-stats fields also stay None for the
        // nested issue shape: serde looks for them at the JSON
        // root, not under time_stats.
        assert!(live_spent.total_time_spent.is_none());
        assert!(live_spent.time_estimate.is_none());

        // Live issue shape for time estimate: confirmed via
        // time_stats.time_estimate.
        let live_estimate: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "id": 7,
                "iid": 2,
                "state": "opened",
                "time_stats": {
                    "time_estimate": 1800,
                    "total_time_spent": 0,
                    "human_time_estimate": "30m",
                    "human_total_time_spent": null
                }
            }"#,
        )
        .unwrap();
        assert!(live_estimate.is_confirmed());
        let stats = live_estimate.time_stats.as_ref().unwrap();
        assert_eq!(stats.time_estimate, Some(1_800));
        assert_eq!(stats.human_time_estimate.as_deref(), Some("30m"));

        // Live GitLab 19.x top-level time-stats response for
        // add_spent_time (confirmed against project 3 issue 5):
        // totals land at the JSON root and the nested time_stats
        // block stays None. is_confirmed must return true via the
        // top-level total_time_spent so the projection advances
        // sync_status to synced.
        let top_level_spent: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "time_estimate": 0,
                "total_time_spent": 6,
                "human_time_estimate": null,
                "human_total_time_spent": "6s"
            }"#,
        )
        .unwrap();
        assert!(top_level_spent.is_confirmed());
        assert_eq!(top_level_spent.total_time_spent, Some(6));
        assert_eq!(
            top_level_spent.human_total_time_spent.as_deref(),
            Some("6s"),
        );
        assert_eq!(top_level_spent.time_estimate, Some(0));
        assert!(top_level_spent.human_time_estimate.is_none());
        // Nested block stays None because the response has no
        // wrapping issue body; legacy flat totals also stay None.
        assert!(top_level_spent.time_stats.is_none());
        assert!(top_level_spent.seconds.is_none());
        assert!(top_level_spent.total_seconds.is_none());

        // Live top-level time-stats response for set_time_estimate:
        // confirmed via top-level time_estimate.
        let top_level_estimate: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "time_estimate": 1800,
                "total_time_spent": 0,
                "human_time_estimate": "30m",
                "human_total_time_spent": null
            }"#,
        )
        .unwrap();
        assert!(top_level_estimate.is_confirmed());
        assert_eq!(top_level_estimate.time_estimate, Some(1_800));
        assert_eq!(
            top_level_estimate.human_time_estimate.as_deref(),
            Some("30m"),
        );
        assert!(top_level_estimate.time_stats.is_none());
    }

    #[test]
    fn spent_time_summary_serialization_keeps_documented_four_key_shape() {
        use super::ApiSpentTimeSummary;

        // The round-trip contract test in gitlab_contract_tests.rs
        // pins the documented 4-key shape. After adding the nested
        // time_stats field, the serialised projection must stay a
        // 4-key object: the field is skipped when None and the
        // decoder never invents a remote id.
        let flat: ApiSpentTimeSummary = serde_json::from_str(
            r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
        )
        .unwrap();
        let value = serde_json::to_value(&flat).unwrap();
        let mut keys = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "human_readable".to_owned(),
                "seconds".to_owned(),
                "total_human_readable".to_owned(),
                "total_seconds".to_owned(),
            ],
            "ApiSpentTimeSummary must not invent an id field beyond the documented totals",
        );

        // Top-level time-stats shape: the four wire-compatibility
        // fields stay None on the legacy keys (the response has no
        // seconds/total_seconds), and the four new top-level
        // fields plus time_stats are deserialised-only. The
        // serialised projection therefore carries no key at all,
        // which is consistent with the documented contract that
        // only the original 4 flat fields may appear.
        let top_level: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "time_estimate": 0,
                "total_time_spent": 6,
                "human_time_estimate": null,
                "human_total_time_spent": "6s"
            }"#,
        )
        .unwrap();
        let value = serde_json::to_value(&top_level).unwrap();
        let keys = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            keys.is_empty(),
            "top-level shape must not serialise any of the wire-compatibility fields: {keys:?}",
        );
    }
}
