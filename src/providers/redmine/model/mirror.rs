use serde::{Deserialize, Serialize};

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

/// Minimal project identity preserved for discovery outcome reporting.
/// Contains only non-sensitive fields needed by Phase 3 to report unique
/// or ambiguous matches without leaking the mirror bearer key or remote
/// credentials.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RedmineDiscoveredProject {
    pub id: u64,
    pub name: String,
    pub identifier: String,
}

/// Read-only discovery result for the current Git origin. `NoMatch` means
/// no visible project had a matching mirror `remote_url`; `Single` is the
/// exactly-one case Phase 3 can use directly; `Multiple` carries all
/// candidates so Phase 3 can fail with an actionable ambiguity error
/// without guessing. Never leaks credentials.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RedmineDiscovery {
    NoMatch,
    Single(RedmineDiscoveredProject),
    Multiple(Vec<RedmineDiscoveredProject>),
}
