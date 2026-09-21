use serde::{Deserialize, Serialize};

/// `iid` is the project-scoped issue number that the orchestrator
/// surfaces as `IssueSummary::number`; the global `id` is recorded but
/// the CLI only uses it for diagnostic logging in the audit comment
/// shape.
///
/// `#[allow(dead_code)]` keeps the Phase 2 widening tolerant to
/// fixtures that do not yet exercise every new field (a future
/// planning flow will read `milestone`, `due_date`, `weight`,
/// `time_stats`, `assignee(s)`, `created_at`, `updated_at`); the
/// decoder stays the single source of truth for the wire shape and
/// the unused fields simply persist as decoded values until the next
/// caller arrives.
#[allow(dead_code)]
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
    #[serde(default)]
    pub milestone: Option<ApiMilestone>,
    /// Phase 2: ISO-8601 date (`YYYY-MM-DD`) or `null`. The
    /// orchestrator surfaces this verbatim in audit comments; it is
    /// not auto-applied to a planning field because GitLab has no
    /// native start/due split on the issue DTO.
    #[serde(default)]
    pub due_date: Option<String>,
    /// Phase 2: integer weight. Premium-only on GitLab.com; the field
    /// is omitted on every non-Premium instance, so the decoder
    /// stays tolerant.
    #[serde(default)]
    pub weight: Option<u64>,
    #[serde(default)]
    pub time_stats: Option<crate::providers::gitlab::model::ApiIssueTimeStats>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Phase 2: assignee usernames. The `assignee` and
    /// `assignees` keys overlap; GitLab 19.x returns the array
    /// shape on the live instance and `assignee` is always `null`
    /// when `assignees` is present. The decoder captures both as
    /// `Option` so a legacy single-user payload still decodes.
    #[serde(default)]
    pub assignee: Option<ApiIssueAssignee>,
    #[serde(default)]
    pub assignees: Option<Vec<ApiIssueAssignee>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct ApiMilestone {
    pub id: u64,
    #[serde(default)]
    pub iid: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `active` or `closed` (GitLab also surfaces `upcoming` for
    /// not-yet-started milestones; the orchestrator treats every
    /// non-`closed` value as open).
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub web_url: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssueAssignee {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiUser {
    pub id: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct NewIssue<'a> {
    pub title: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub description: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assignee_ids: Vec<u64>,
}

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

#[derive(Debug, Deserialize)]
pub(crate) struct ApiNote {
    pub id: u64,
    pub body: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct NewNote<'a> {
    pub body: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ApiLabel {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct NewLabel<'a> {
    pub name: &'a str,
    pub color: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
}

#[allow(dead_code)]
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
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiProjectNamespace {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub full_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiNamespace {
    pub id: u64,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub initialize_with_readme: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiError {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}
