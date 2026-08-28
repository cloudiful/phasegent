//! GitLab API DTOs and request payloads.

use serde::{Deserialize, Serialize};

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
