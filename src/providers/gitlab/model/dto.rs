//! GitLab API DTOs and request payloads.

use serde::{Deserialize, Serialize};

/// JSON payload returned by `GET /projects/:id/issues/:iid`.
///
/// Phase 2 widens the decoder to accept every documented GitLab issue
/// field the orchestrator ever inspects in audit comments or future
/// planning flows. All newly-added fields are `Option`/`default`-skipped
/// so existing call sites (and older fixture payloads) keep decoding
/// unchanged. The original narrow shape (`id`, `iid`, `title`,
/// `description`, `state`, `labels`, `web_url`) stays required because
/// it is the contract every consumer relied on before Phase 2.
///
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
    /// Phase 2: milestone the issue belongs to. None when the issue is
    /// not assigned to a milestone. The shape mirrors GitLab's nested
    /// `milestone` object (`{id, iid, project_id, title, state,
    /// due_date, ...}`); only the fields the audit comment and future
    /// planning readers actually consume are decoded, and unknown
    /// fields are silently ignored so a future GitLab payload extension
    /// never breaks the client.
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
    /// Phase 2: time stats block carried by the GitLab issue payload.
    /// Reuses [`crate::providers::gitlab::model::time::ApiIssueTimeStats`]
    /// so the spent-time / time-estimate decoders keep their
    /// `is_confirmed` short-circuit on the same struct.
    #[serde(default)]
    pub time_stats: Option<crate::providers::gitlab::model::ApiIssueTimeStats>,
    /// Phase 2: creation timestamp in GitLab's ISO-8601 format.
    /// Surfaced in audit comments so the orchestrator can render
    /// when the issue was filed.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Phase 2: last-modified timestamp in GitLab's ISO-8601 format.
    /// Surfaced in audit comments so the orchestrator can render
    /// when the issue was last touched.
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

/// Nested milestone payload carried by `ApiIssue`. Mirrors the
/// documented GitLab response; unknown fields are silently dropped so
/// a future GitLab release that adds milestone metadata does not break
/// the client. `id` is required because `milestone.id` is the
/// identifier the orchestrator surfaces (mirroring `RedmineVersion`).
///
/// `#[allow(dead_code)]` keeps the extra payload fields (`iid`,
/// `description`, `start_date`, `web_url`) tolerant to call sites
/// that only consume `id`, `title`, `state`, and `due_date`; the
/// decoder remains the single source of truth for the wire shape and
/// a future planning flow can adopt the remaining fields without
/// changing the model.
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

/// Nested assignee payload carried by `ApiIssue`. The orchestrator only
/// surfaces the username and name so audit comments stay readable; the
/// numeric `id` is kept because future planning flows may resolve
/// assignees by it.
///
/// `#[allow(dead_code)]` keeps the decoder tolerant while no live
/// consumer reads the fields; future planning flows can adopt them
/// without changing the model.
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

/// JSON payload returned by `POST /projects`, `GET /projects/:id`,
/// and `GET /projects` (list).
///
/// GitLab echoes the namespace as a nested object (`{ "id": …, "path": …,
/// "full_path": …, "kind": "user"|"group" }`); only `path` and `full_path`
/// matter for the orchestrator's repository summary. `name` and `path` are
/// kept separate because GitLab uses `path` as the URL slug.
///
/// Phase 2 widens the decoder so `list_projects` can map a GitLab
/// project onto the shared `RedmineProject` shape. The original narrow
/// fields (`path`, `path_with_namespace`, `web_url`, `visibility`,
/// `namespace`, `http_url_to_repo`, `ssh_url_to_repo`) stay required
/// for the `repo create` path; the new fields are all `Option`/
/// `default`-skipped so older fixture payloads still decode cleanly.
///
/// `#[allow(dead_code)]` keeps the audit-only fields (`default_branch`,
/// `archived`) tolerant to call sites that only consume `id`, `name`,
/// `description`, `path`, and `visibility`; the decoder remains the
/// single source of truth for the wire shape.
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
    /// Phase 2: numeric project id (mirrors `RedmineProject::id`).
    /// Required for the `list_projects` mapping; missing on
    /// pre-existing fixtures that only used the narrow path. The
    /// `repo create` POST response always carries it.
    #[serde(default)]
    pub id: Option<u64>,
    /// Phase 2: human-readable project name (mirrors
    /// `RedmineProject::name`). GitLab returns both `name` and
    /// `path`; the orchestrator surfaces `name` for display and
    /// `path` as the Redmine identifier slug.
    #[serde(default)]
    pub name: Option<String>,
    /// Phase 2: project description. GitLab returns `null` when
    /// the project has no description, which decodes to `None`;
    /// the list mapping substitutes an empty string to mirror the
    /// Redmine shape.
    #[serde(default)]
    pub description: Option<String>,
    /// Phase 2: ISO-8601 creation timestamp.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Phase 2: ISO-8601 last-modified timestamp.
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Phase 2: default branch name. Audit-only; not surfaced in
    /// `RedmineProject`.
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Phase 2: archived flag. Audit-only.
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
