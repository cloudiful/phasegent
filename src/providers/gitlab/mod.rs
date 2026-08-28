//! GitLab provider implementation: issue CRUD, note lifecycle, label
//! management, tracker mapping, and workflow status updates.
//!
//! The provider is the only place that knows about GitLab's HTTP
//! shape. Higher layers (`provider_dispatch.rs`,
//! `redmine_planning_cli.rs`) interact with it through the shared
//! `IssueProvider` trait and a small set of GitLab-specific helpers
//! for label / workflow operations.
//!
//! Phase 2 deliberately leaves a handful of capabilities as
//! structured not-supported stubs:
//!   - repository creation (Phase 3)
//!   - CI runs / jobs / logs (Phase 3)
//!   - project enumeration and creation (Phase 3)
//!   - planning fields (parent issue, fixed version, dates, estimates,
//!     done ratio) - mapped to a Redmine-only planning CLI today, so a
//!     caller that asks for one against GitLab gets a structured
//!     not-supported error before any network access.

pub mod http;
pub mod model;

#[cfg(test)]
mod contract_tests;

use crate::ci_model::{
    CiInspectOutput, CiInspectRequest, CiJobLogsOutput, CiJobsOutput, CiRunSummary, CiRunsFilter,
    CiRunsOutput, bound_log, pretty_ref as shared_pretty_ref,
};
use crate::infra::storage::Storage;
use crate::policy::Capability;
use crate::providers::api::{CommentOutput, ForgejoError, IssueSummary, RepoSummary};
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::http::GitlabHttp;
use crate::providers::gitlab::model::{
    ApiIssue, ApiIssueLink, ApiJob, ApiLabel, ApiNamespace, ApiNote, ApiPipeline, ApiProject,
    ApiSpentTimeSummary, NewIssue, NewLabel, NewNote, NewProject, NewSpentTime, NewTimeEstimate,
    TRACKER_LABEL_BUG, TRACKER_LABEL_FEATURE, UpdateIssue, WORKFLOW_LABELS, format_gitlab_duration,
    gitlab_link_type_from_relation_type, pipeline_conclusion_from_gitlab,
    pipeline_status_from_gitlab, state_from_gitlab, state_query_filter, tracker_label_from_name,
    tracker_name_from_label, workflow_label_from_status,
};
use crate::providers::redmine::model::{RedmineRelationType, RelationSummary};

/// Concrete GitLab provider. The struct is held by the
/// `ProviderDispatcher::Gitlab` arm; the surrounding CLI talks to it
/// through the shared `IssueProvider` trait and a handful of GitLab-
/// specific helpers (`set_workflow_status`, `tracker_label`, etc.).
///
/// Public so `provider_config.rs` can re-export it under the same name
/// for the `crate::providers::ProviderDispatcher` enum. The struct is
/// an opaque transport; callers should drive it via the trait, not
/// reach into its fields, so widening visibility here does not
/// expose anything new beyond what the dispatcher already surfaces.
#[allow(dead_code)]
pub struct GitlabProvider {
    pub(crate) config: GitlabConfig,
    pub(crate) http: GitlabHttp,
}

impl std::fmt::Debug for GitlabProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The HTTP struct already redacts the token in its `Debug`
        // impl; delegate to it so the provider-level Debug cannot
        // accidentally expose it either.
        formatter
            .debug_struct("GitlabProvider")
            .field("config", &self.config)
            .field("http", &self.http)
            .finish()
    }
}

impl GitlabProvider {
    pub(crate) fn for_role(
        role: crate::policy::Role,
        config: GitlabConfig,
    ) -> Result<Self, ForgejoError> {
        let storage = Storage::open().map_err(ForgejoError::config)?;
        let token = crate::auth::gitlab_token(role, &storage).map_err(ForgejoError::auth)?;
        Self::new(config, token)
    }

    pub(crate) fn new(config: GitlabConfig, token: String) -> Result<Self, ForgejoError> {
        let http = GitlabHttp::new(config.api_base.clone(), token)?;
        Ok(Self { config, http })
    }

    /// Numeric project id used in every per-project URL.
    pub(crate) const fn project_id(&self) -> u64 {
        self.config.project_id
    }

    /// Borrow of the resolved `/api/v4` base URL.
    // -- HTTP path builders --------------------------------------------------
    fn issues_path(&self) -> String {
        format!("projects/{}/issues", self.project_id())
    }

    fn issue_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}", self.project_id())
    }

    fn notes_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}/notes", self.project_id())
    }

    fn note_path(&self, iid: u64, note_id: u64) -> String {
        format!(
            "projects/{}/issues/{iid}/notes/{note_id}",
            self.project_id()
        )
    }

    fn labels_path(&self) -> String {
        format!("projects/{}/labels", self.project_id())
    }

    fn spent_time_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}/add_spent_time", self.project_id())
    }

    fn time_estimate_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}/time_estimate", self.project_id())
    }

    fn issue_links_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}/links", self.project_id())
    }

    fn issue_link_path(&self, source_issue_iid: u64, link_id: u64) -> String {
        format!(
            "projects/{}/issues/{source_issue_iid}/links/{link_id}",
            self.project_id()
        )
    }

    // -- Issue lifecycle ----------------------------------------------------

    /// `GET /projects/:id/issues/:iid` - one issue by its project-
    /// scoped `iid`.
    pub(crate) fn get_issue(&self, iid: u64) -> Result<IssueSummary, ForgejoError> {
        let issue: ApiIssue = self.http.get(&self.issue_path(iid), &[], "issue get")?;
        Ok(issue.into_summary(self))
    }

    /// `GET /projects/:id/issues/:iid` returning the raw `ApiIssue`
    /// so callers that must inspect the full label set (the
    /// [`IssueSummary`] view strips it) can do so without a second
    /// network call. Used by the label-replacement path in
    /// [`update_body_with_labels`](Self::update_body_with_labels) to
    /// detect the opposite managed tracker label.
    fn get_raw_issue(&self, iid: u64) -> Result<ApiIssue, ForgejoError> {
        self.http.get(&self.issue_path(iid), &[], "issue get")
    }

    /// `GET /projects/:id/issues?state=...&search=...&per_page=50&page=N`
    /// paginated until GitLab signals completion via a partial page
    /// or the safety cap. The shared `open` / `closed` / `all`
    /// selector is translated to GitLab's `opened` / `closed` /
    /// omitted state filter.
    pub(crate) fn search_issues(
        &self,
        query: Option<&str>,
        state: &str,
    ) -> Result<Vec<IssueSummary>, ForgejoError> {
        let state_filter = state_query_filter(state)?;
        let path = self.issues_path();
        let issues = self.http.paginate("issue search", |http, page| {
            let mut params = vec![("page", page.to_string())];
            if let Some(filter) = state_filter {
                params.push(("state", filter.to_owned()));
            }
            if let Some(query) = query.filter(|value| !value.is_empty()) {
                params.push(("search", query.to_owned()));
            }
            http.get_page::<ApiIssue>(&path, &params, "issue search")
        })?;
        Ok(issues
            .into_iter()
            .map(|issue| issue.into_summary(self))
            .collect())
    }

    /// `POST /projects/:id/issues` with an optional `labels` field
    /// for tracker mapping.
    pub(crate) fn create_issue_with_labels(
        &self,
        title: &str,
        description: &str,
        labels: &[String],
    ) -> Result<IssueSummary, ForgejoError> {
        let payload = NewIssue {
            title,
            description,
            labels: labels.to_vec(),
        };
        let issue: ApiIssue = self
            .http
            .post(&self.issues_path(), &payload, "issue create")?;
        Ok(issue.into_summary(self))
    }

    /// Plain create without label manipulation. Mirrors the shared
    /// `IssueProvider::create_issue` signature so the trait impl can
    /// delegate cleanly; the planning-aware CLI path uses
    /// [`create_issue_with_labels`] directly when a tracker is
    /// supplied.
    pub(crate) fn create_issue(
        &self,
        title: &str,
        body: &str,
    ) -> Result<IssueSummary, ForgejoError> {
        self.create_issue_with_labels(title, body, &[])
    }

    /// `PUT /projects/:id/issues/:iid` with an optional description
    /// and label delta.
    ///
    /// When `labels` includes a managed tracker label (`type::bug` /
    /// `type::feature`), the opposite tracker label is added to
    /// `remove_labels` if the issue currently carries it so the issue
    /// never ends up holding both. Workflow labels and unrelated
    /// project labels are preserved untouched.
    pub(crate) fn update_body_with_labels(
        &self,
        iid: u64,
        description: &str,
        labels: &[String],
    ) -> Result<IssueSummary, ForgejoError> {
        // Ensure every label we are about to add already exists in
        // the project before referencing it; GitLab rejects a PUT
        // for an unknown label.
        if !labels.is_empty() {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            self.ensure_labels(&refs)?;
        }
        // Fetch the raw issue so we can inspect the full label set
        // before deciding which managed tracker label to remove.
        let current = self.get_raw_issue(iid)?;
        let mut remove_labels: Vec<String> = Vec::new();
        for added in labels {
            if let Some(opposite) = Self::opposite_tracker_label(added) {
                let already_added = labels.iter().any(|value| value == opposite);
                let currently_attached = current.labels.iter().any(|value| value == opposite);
                if currently_attached && !already_added {
                    remove_labels.push(opposite.to_owned());
                }
            }
        }
        let payload = UpdateIssue {
            description: Some(description),
            state_event: None,
            add_labels: labels.to_vec(),
            remove_labels,
        };
        let response: Option<ApiIssue> =
            self.http
                .put(&self.issue_path(iid), &payload, "issue update-body")?;
        parse_optional_issue(response, "issue update-body").map(|issue| issue.into_summary(self))
    }

    /// Plain body update; no label delta. Used when a caller only
    /// updates the description and explicitly does not want to
    /// disturb the current label set.
    pub(crate) fn update_body(&self, iid: u64, body: &str) -> Result<IssueSummary, ForgejoError> {
        let payload = UpdateIssue {
            description: Some(body),
            state_event: None,
            add_labels: Vec::new(),
            remove_labels: Vec::new(),
        };
        let response: Option<ApiIssue> =
            self.http
                .put(&self.issue_path(iid), &payload, "issue update-body")?;
        parse_optional_issue(response, "issue update-body").map(|issue| issue.into_summary(self))
    }

    /// Close an issue via the native `state_event=close` field plus
    /// the `workflow::closed` label so the orchestrator's existing
    /// status invariants still hold.
    pub(crate) fn close_issue(&self, iid: u64) -> Result<IssueSummary, ForgejoError> {
        self.apply_status(iid, Some("workflow::closed"), true)
    }

    // -- Note lifecycle -----------------------------------------------------

    /// `POST /projects/:id/issues/:iid/notes` with the caller's body.
    /// GitLab note ids and URLs are stable, so the returned
    /// `CommentOutput` carries both.
    pub(crate) fn create_note(&self, iid: u64, body: &str) -> Result<CommentOutput, ForgejoError> {
        let payload = NewNote { body };
        let note: ApiNote = self
            .http
            .post(&self.notes_path(iid), &payload, "comment create")?;
        // GitLab renders notes inline on the parent issue page, so
        // the canonical note URL is `<issue_web_url>#note_<id>`.
        // Fetch the issue to obtain its `web_url`; if the API omits
        // it, fall back to `None` rather than synthesising an API
        // path that is not browsable.
        let issue_web_url = self.get_issue(iid)?.html_url;
        Ok(note.into_output(issue_web_url.as_deref()))
    }

    /// `GET /projects/:id/issues/:iid/notes/:note_id` for one note.
    pub(crate) fn get_note(&self, iid: u64, note_id: u64) -> Result<CommentOutput, ForgejoError> {
        let note: ApiNote = self
            .http
            .get(&self.note_path(iid, note_id), &[], "comment get")?;
        let issue_web_url = self.get_issue(iid)?.html_url;
        Ok(note.into_output(issue_web_url.as_deref()))
    }

    /// `GET /projects/:id/issues/:iid/notes` paginated until
    /// completion. System notes (`system: true`) are still walked,
    /// but only matched against the marker if the body contains it,
    /// which is extremely unlikely for system events. The marker
    /// lookup therefore never accidentally returns a system note as
    /// an audit note unless the operator deliberately embedded the
    /// marker in such a note.
    pub(crate) fn find_marker(
        &self,
        iid: u64,
        marker: &str,
    ) -> Result<CommentOutput, ForgejoError> {
        if marker.is_empty() {
            return Err(ForgejoError::config("marker cannot be empty"));
        }
        // Resolve the parent issue once so every returned note can
        // carry a browsable `<issue_web_url>#note_<id>` URL.
        let issue_web_url = self.get_issue(iid)?.html_url;
        let path = self.notes_path(iid);
        let notes = self.http.paginate("comment list", |http, page| {
            http.get_page::<ApiNote>(&path, &[("page", page.to_string())], "comment list")
        })?;
        notes
            .into_iter()
            .find(|note| note.body.contains(marker))
            .map(|note| {
                note.into_output(issue_web_url.as_deref())
                    .with_marker(marker.to_owned())
            })
            .ok_or_else(|| ForgejoError::not_found("comment find-marker", "marker was not found"))
    }

    // -- Label / workflow helpers ------------------------------------------

    /// Resolve the issue's current label set, ensuring every managed
    /// workflow and tracker label exists in the project. Returns the
    /// final, fully-attached label list. Used both for read paths
    /// (so the orchestrator can render the issue with up-to-date
    /// labels) and for the workflow update path (so the managed
    /// labels are guaranteed to exist before they are referenced by
    /// the issue update payload).
    pub(crate) fn ensure_labels(&self, labels: &[&str]) -> Result<Vec<String>, ForgejoError> {
        let existing = self.list_project_labels()?;
        let mut ensured = Vec::with_capacity(labels.len());
        for name in labels {
            if existing.iter().any(|candidate| candidate.name == *name) {
                ensured.push((*name).to_owned());
                continue;
            }
            self.create_label(name)?;
            ensured.push((*name).to_owned());
        }
        Ok(ensured)
    }

    fn list_project_labels(&self) -> Result<Vec<ApiLabel>, ForgejoError> {
        let path = self.labels_path();
        self.http.paginate("label list", |http, page| {
            http.get_page::<ApiLabel>(&path, &[("page", page.to_string())], "label list")
        })
    }

    fn create_label(&self, name: &str) -> Result<ApiLabel, ForgejoError> {
        let payload = NewLabel {
            name,
            color: label_color(name),
            description: None,
        };
        self.http
            .post(&self.labels_path(), &payload, "label create")
    }

    /// Apply the orchestrator's `workflow_status` value to an issue:
    /// removes every prior `workflow::*` label, ensures the target
    /// workflow label exists, and pairs `closed` with the native
    /// `state_event=close` (or `state_event=reopen` for every other
    /// target so a previously-closed issue can be re-opened).
    pub(crate) fn set_workflow_status(
        &self,
        iid: u64,
        status: &str,
    ) -> Result<IssueSummary, ForgejoError> {
        let label = workflow_label_from_status(status)?;
        let is_closed = label == "workflow::closed";
        self.apply_status(iid, Some(label), is_closed)
    }

    fn apply_status(
        &self,
        iid: u64,
        label: Option<&str>,
        is_closed: bool,
    ) -> Result<IssueSummary, ForgejoError> {
        // Ensure the target label exists before referencing it.
        if let Some(label) = label {
            self.ensure_labels(&[label])?;
        }
        // Fetch the current issue so we only emit `state_event` when
        // the issue actually needs to transition. GitLab REST v4
        // rejects state_event=reopen on an already-open issue and
        // state_event=close on an already-closed issue with HTTP 400,
        // so emitting the field unconditionally would break the
        // idempotent `status set` path.
        let current = self.get_issue(iid)?;
        let state_event = match (is_closed, current.state.as_str()) {
            (true, "closed") => None,
            (true, _) => Some("close"),
            (false, "closed") => Some("reopen"),
            (false, _) => None,
        };
        // Build the PUT payload: clear every workflow::* label, add
        // the target label, optionally toggle the native state.
        let payload = UpdateIssue {
            description: None,
            state_event,
            add_labels: label
                .map(|value| vec![value.to_owned()])
                .unwrap_or_default(),
            remove_labels: WORKFLOW_LABELS
                .iter()
                .filter(|candidate| label.is_none_or(|target| **candidate != target))
                .map(|value| (*value).to_owned())
                .collect(),
        };
        let response: Option<ApiIssue> =
            self.http
                .put(&self.issue_path(iid), &payload, "issue status update")?;
        parse_optional_issue(response, "issue status update").map(|issue| issue.into_summary(self))
    }

    /// Resolve a raw `--tracker` value to a GitLab label and ensure
    /// the label exists in the project. Returns the label name ready
    /// for inclusion in a create or update payload.
    pub(crate) fn tracker_label(&self, value: &str) -> Result<String, ForgejoError> {
        let label = tracker_label_from_name(value)?;
        if tracker_name_from_label(label).is_none() {
            return Err(ForgejoError::config(
                "GitLab tracker label mapping is incomplete",
            ));
        }
        self.ensure_labels(&[label])?;
        Ok(label.to_owned())
    }

    /// Resolve `--tracker Bug|Feature` to a label list (one element).
    pub(crate) fn tracker_label_list(&self, value: &str) -> Result<Vec<String>, ForgejoError> {
        Ok(vec![self.tracker_label(value)?])
    }

    /// Map a managed tracker label to its opposite managed label, or
    /// `None` for any other label. Used by
    /// [`update_body_with_labels`](Self::update_body_with_labels) so
    /// switching from one tracker to the other does not leave both
    /// `type::bug` and `type::feature` attached to the same issue.
    fn opposite_tracker_label(label: &str) -> Option<&'static str> {
        match label {
            TRACKER_LABEL_BUG => Some(TRACKER_LABEL_FEATURE),
            TRACKER_LABEL_FEATURE => Some(TRACKER_LABEL_BUG),
            _ => None,
        }
    }

    // -- Not-supported helpers for later phases ------------------------------

    #[allow(dead_code)]
    fn unsupported<T>(&self, operation: &str) -> Result<T, ForgejoError> {
        Err(ForgejoError::not_supported("gitlab", operation))
    }

    // -- Time tracking -------------------------------------------------------

    /// `POST /projects/:id/issues/:iid/add_spent_time` with a GitLab
    /// human-format duration and an optional run-marker summary. The
    /// duration is validated locally so a non-positive value never
    /// reaches the wire. The endpoint returns a summary object; the
    /// `ApiSpentTimeSummary` decoder keeps the running totals so the
    /// caller can render the updated spent time without a follow-up
    /// GET.
    pub(crate) fn add_spent_time(
        &self,
        iid: u64,
        duration_seconds: i64,
        summary: Option<&str>,
    ) -> Result<ApiSpentTimeSummary, ForgejoError> {
        if iid == 0 {
            return Err(ForgejoError::config(
                "GitLab issue iid must be greater than zero",
            ));
        }
        if duration_seconds <= 0 {
            return Err(ForgejoError::config("GitLab spent time must be positive"));
        }
        let duration = format_gitlab_duration(duration_seconds);
        let payload = NewSpentTime {
            duration: &duration,
            summary,
        };
        self.http
            .post(&self.spent_time_path(iid), &payload, "time spent create")
    }

    /// `POST /projects/:id/issues/:iid/time_estimate` with a GitLab
    /// human-format duration. Mirrors the spent time contract: the
    /// duration is validated locally before it reaches the wire so a
    /// non-positive value surfaces as a structured config error.
    pub(crate) fn set_time_estimate(
        &self,
        iid: u64,
        duration_seconds: i64,
    ) -> Result<ApiSpentTimeSummary, ForgejoError> {
        if iid == 0 {
            return Err(ForgejoError::config(
                "GitLab issue iid must be greater than zero",
            ));
        }
        if duration_seconds <= 0 {
            return Err(ForgejoError::config(
                "GitLab time estimate must be positive",
            ));
        }
        let duration = format_gitlab_duration(duration_seconds);
        let payload = NewTimeEstimate {
            duration: &duration,
        };
        self.http
            .post(&self.time_estimate_path(iid), &payload, "time estimate set")
    }

    // -- Issue links ---------------------------------------------------------

    /// `GET /projects/:id/issues/:iid/links` paginated until GitLab
    /// signals completion. The CLI only needs a single page for
    /// realistic graph sizes, but the paginated helper stays
    /// symmetrical with the issue and note paths. The returned
    /// summaries are rendered from the queried issue's viewpoint so
    /// `is_blocked_by` shows up as `blocked` (the inverse Redmine
    /// name).
    pub(crate) fn list_issue_links(&self, iid: u64) -> Result<Vec<RelationSummary>, ForgejoError> {
        if iid == 0 {
            return Err(ForgejoError::config(
                "GitLab issue iid must be greater than zero",
            ));
        }
        let path = self.issue_links_path(iid);
        let links = self.http.paginate("relation list", |http, page| {
            http.get_page::<ApiIssueLink>(&path, &[("page", page.to_string())], "relation list")
        })?;
        Ok(links
            .into_iter()
            .map(|link| link.into_summary(iid))
            .collect())
    }

    /// `POST /projects/:id/issues/:iid/links` with the canonical CLI
    /// `--type` mapped to GitLab's `link_type` spelling. The
    /// `RedmineRelationType` enum is the single source of truth so the
    /// parser already rejected any invalid names.
    ///
    /// The live `https://gitlab.example.com/19.2` instance
    /// rejects the body-shape payload and only accepts the request
    /// when `target_project_id`, `target_issue_iid`, and the
    /// optional `link_type` are sent as URL query parameters. We
    /// route the call through the [`GitlabHttp::post_with_query`]
    /// helper so the credentials stay in the `PRIVATE-TOKEN` header
    /// rather than the URL, and the body is left empty.
    ///
    /// The same instance only accepts `relates_to` for create:
    /// `blocks` and `is_blocked_by` come back with
    /// `link_type does not have a valid value` even when the query
    /// parameters are correct. We gate the create path locally via
    /// [`gitlab_model::gitlab_create_supports_relation_type`] so
    /// the unsupported directions fail with a structured
    /// [`ForgejoError::NotSupported`] error before any network
    /// traffic. The list mapping still decodes every
    /// server-returned direction (`blocks`, `is_blocked_by`).
    pub(crate) fn create_issue_link(
        &self,
        issue_iid: u64,
        target_iid: u64,
        relation_type: RedmineRelationType,
    ) -> Result<RelationSummary, ForgejoError> {
        if issue_iid == 0 {
            return Err(ForgejoError::config(
                "GitLab issue iid must be greater than zero",
            ));
        }
        if target_iid == 0 {
            return Err(ForgejoError::config(
                "GitLab target issue iid must be greater than zero",
            ));
        }
        if issue_iid == target_iid {
            return Err(ForgejoError::config(
                "GitLab issue link cannot target the same issue",
            ));
        }
        if !crate::providers::gitlab::model::gitlab_create_supports_relation_type(relation_type) {
            // The live instance rejects every direction other than
            // `relates_to` with a structured validation error. Fail
            // before any HTTP traffic so the client sees a
            // consistent not-supported error regardless of which
            // direction the caller asked for, and so the read path
            // remains the only place that decodes `blocks` /
            // `is_blocked_by` from a server response.
            return Err(ForgejoError::not_supported(
                "gitlab",
                "relation create with the requested link_type",
            ));
        }
        let link_type = gitlab_link_type_from_relation_type(relation_type)?;
        let query = vec![
            ("target_project_id", self.project_id().to_string()),
            ("target_issue_iid", target_iid.to_string()),
            ("link_type", link_type.to_owned()),
        ];
        let link: ApiIssueLink = self.http.post_with_query(
            &self.issue_links_path(issue_iid),
            None::<&()>,
            &query,
            "relation create",
        )?;
        Ok(link.into_summary(issue_iid))
    }

    /// `DELETE /projects/:id/issues/:issue_iid/links/:link_id`. The
    /// source issue iid is required by the GitLab REST v4 contract
    /// (the endpoint is scoped per source issue and there is no
    /// single-link GET that resolves the source from a link id).
    /// `source_issue_iid` is therefore an explicit parameter; callers
    /// that have no source context must surface a structured config
    /// error rather than silently guessing the source.
    pub(crate) fn delete_issue_link(
        &self,
        source_issue_iid: Option<u64>,
        link_id: u64,
    ) -> Result<u64, ForgejoError> {
        let source_issue_iid = source_issue_iid.ok_or_else(|| {
            ForgejoError::config(
                "GitLab relation delete requires the source issue iid; \
                 the DELETE endpoint is scoped per source issue and the \
                 GitLab REST v4 API exposes no single-link GET that \
                 resolves the source from a link id. Forward --issue \
                 <SOURCE_ISSUE_IID> from the relation delete CLI once \
                 it is wired through the parser.",
            )
        })?;
        if source_issue_iid == 0 {
            return Err(ForgejoError::config(
                "GitLab source issue iid must be greater than zero",
            ));
        }
        if link_id == 0 {
            return Err(ForgejoError::config(
                "GitLab issue link id must be greater than zero",
            ));
        }
        let path = self.issue_link_path(source_issue_iid, link_id);
        let _: Option<serde_json::Value> = self.http.delete(&path, "relation delete")?;
        Ok(link_id)
    }

    pub(crate) fn list_projects(
        &self,
    ) -> Result<Vec<crate::providers::redmine::model::RedmineProject>, ForgejoError> {
        // GitLab project enumeration is part of Phase 3; surface the
        // structured not-supported error so callers do not silently
        // see a Redmine-shaped result.
        let _ = self;
        Err(ForgejoError::not_supported("gitlab", "project list"))
    }

    // -- Repository creation ------------------------------------------------

    // -- HTTP path builders --------------------------------------------------

    fn projects_path(&self) -> String {
        "projects".to_owned()
    }

    fn pipelines_path(&self) -> String {
        format!("projects/{}/pipelines", self.project_id())
    }

    fn pipeline_path(&self, pipeline_id: u64) -> String {
        format!("projects/{}/pipelines/{pipeline_id}", self.project_id())
    }

    fn pipeline_jobs_path(&self, pipeline_id: u64) -> String {
        format!(
            "projects/{}/pipelines/{pipeline_id}/jobs",
            self.project_id()
        )
    }

    fn job_trace_path(&self, job_id: u64) -> String {
        format!("projects/{}/jobs/{job_id}/trace", self.project_id())
    }

    /// Resolve an authenticated-user namespace id from GitLab. Phase 3
    /// deliberately resolves namespaces lazily: when the orchestrator
    /// passes a bare `REPOSITORY` (no `OWNER/` prefix), this method
    /// fetches the current user via `/user` and returns its numeric
    /// id so the project lands in the caller's personal namespace,
    /// matching the Forgejo behaviour.
    fn current_user_id(&self) -> Result<u64, ForgejoError> {
        #[derive(serde::Deserialize)]
        struct CurrentUser {
            id: u64,
        }
        let user: CurrentUser = self.http.get("user", &[], "repo create")?;
        Ok(user.id)
    }

    /// Resolve an `OWNER` path to a numeric GitLab namespace id by
    /// searching `/namespaces?search=OWNER`. The endpoint returns
    /// namespaces whose path contains the search term; the resolver
    /// filters to exact matches and distinguishes `user` from
    /// `group` namespaces so the operator never lands a project
    /// under the wrong namespace:
    ///
    ///   * zero matches → structured config error (the operator
    ///     must pick a different OWNER or pass an explicit id).
    ///   * exactly one matching `group` → use that group id
    ///     (groups are the typical meaning of `OWNER/REPO`).
    ///   * exactly one matching `user` → use that user id (handles
    ///     cross-account repos that target another user's namespace).
    ///   * any other combination (multiple groups, multiple users,
    ///     or a mix of both kinds) → structured config error
    ///     instructing the operator to disambiguate.
    fn resolve_owner_namespace_id(&self, owner: &str) -> Result<u64, ForgejoError> {
        if owner.is_empty() {
            return Err(ForgejoError::config(
                "GitLab repo create requires a non-empty OWNER",
            ));
        }
        let candidates: Vec<ApiNamespace> = self.http.paginate("repo create", |http, page| {
            http.get_page::<ApiNamespace>(
                "namespaces",
                &[("search", owner.to_owned()), ("page", page.to_string())],
                "repo create",
            )
        })?;
        let exact: Vec<&ApiNamespace> = candidates
            .iter()
            .filter(|namespace| namespace.path.as_deref() == Some(owner))
            .collect();
        if exact.is_empty() {
            return Err(ForgejoError::config(format!(
                "GitLab namespace '{owner}' was not found; pass a different OWNER \
                 or supply an explicit namespace id"
            )));
        }
        let groups: Vec<u64> = exact
            .iter()
            .filter(|namespace| namespace.kind.as_deref() == Some("group"))
            .map(|namespace| namespace.id)
            .collect();
        let users: Vec<u64> = exact
            .iter()
            .filter(|namespace| namespace.kind.as_deref() == Some("user"))
            .map(|namespace| namespace.id)
            .collect();
        if !groups.is_empty() && users.is_empty() {
            if groups.len() == 1 {
                return Ok(groups[0]);
            }
            return Err(ForgejoError::config(format!(
                "GitLab group namespace '{owner}' is ambiguous \
                 (matched {} groups); pass an explicit namespace id",
                groups.len()
            )));
        }
        if groups.is_empty() && !users.is_empty() {
            if users.len() == 1 {
                return Ok(users[0]);
            }
            return Err(ForgejoError::config(format!(
                "GitLab user namespace '{owner}' is ambiguous \
                 (matched {} users); pass an explicit namespace id",
                users.len()
            )));
        }
        Err(ForgejoError::config(format!(
            "GitLab namespace '{owner}' is ambiguous (matched {} group(s) and {} user(s)); \
             pass an explicit namespace id to disambiguate",
            groups.len(),
            users.len()
        )))
    }

    /// Map the orchestrator's `OWNER/REPOSITORY` target to either an
    /// explicit `namespace_id` (when the owner resolves to a known
    /// group or user) or the authenticated user's namespace id. The
    /// function never returns a guessed namespace id; when the
    /// caller passes a bare `REPOSITORY` (no slash) the function
    /// returns the user's namespace id so a project lands in the
    /// caller's personal namespace, matching Forgejo's behaviour.
    ///
    /// The `(Some(namespace), None)` arm leaves `namespace_id`
    /// unset so the caller (currently [`GitlabProvider::create_repo`])
    /// resolves the owner via [`Self::resolve_owner_namespace_id`]
    /// before issuing POST `/projects`. The pure helper has no
    /// network access of its own.
    pub(crate) fn resolve_namespace_target(
        target: &str,
        explicit_namespace_id: Option<u64>,
        current_user_id: u64,
    ) -> Result<ResolvedNamespace, ForgejoError> {
        if target.is_empty() {
            return Err(ForgejoError::config(
                "GitLab repo create requires a non-empty target",
            ));
        }
        let (namespace, path) = match target.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() => {
                (Some(owner.to_owned()), name.to_owned())
            }
            Some((owner, name)) if owner.is_empty() && !name.is_empty() => (None, name.to_owned()),
            _ => (None, target.to_owned()),
        };
        let namespace_id = match (namespace, explicit_namespace_id) {
            (Some(_), Some(id)) => Some(id),
            (None, Some(id)) => Some(id),
            (None, None) => Some(current_user_id),
            (Some(_), None) => None,
        };
        Ok(ResolvedNamespace { namespace_id, path })
    }

    /// `POST /projects` with the orchestrator's repo create payload.
    /// Private-only by contract; the caller must already have
    /// validated `--private` so a non-private call surfaces a
    /// structured error before any network traffic.
    pub(crate) fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, ForgejoError> {
        if !private {
            return Err(ForgejoError::config(
                "repo create requires a private repository",
            ));
        }
        let current_user_id = self.current_user_id()?;
        let mut resolved = Self::resolve_namespace_target(target, None, current_user_id)?;
        if resolved.namespace_id.is_none() {
            // The caller passed OWNER/REPO without an explicit
            // namespace id; resolve OWNER to its numeric id via the
            // authenticated namespaces endpoint so POST /projects
            // cannot silently fall back to the personal namespace.
            let owner = target.split_once('/').map(|(owner, _)| owner).unwrap_or("");
            resolved.namespace_id = Some(self.resolve_owner_namespace_id(owner)?);
        }
        let payload = NewProject {
            name: &resolved.path,
            path: Some(&resolved.path),
            namespace_id: resolved.namespace_id,
            namespace: None,
            visibility: "private",
            description,
            initialize_with_readme: auto_init,
        };
        let project: ApiProject = self
            .http
            .post(&self.projects_path(), &payload, "repo create")?;
        Ok(project.into_summary())
    }

    // -- CI reads ------------------------------------------------------------

    /// `GET /projects/:id/pipelines` paginated until GitLab signals
    /// completion. `ref`, `sha`, and `status` are forwarded as
    /// query parameters so the orchestrator's `CiRunsFilter` maps
    /// cleanly onto GitLab's filters.
    pub(crate) fn ci_runs(&self, filter: &CiRunsFilter) -> Result<CiRunsOutput, ForgejoError> {
        let path = self.pipelines_path();
        let pipelines = self.http.paginate("ci runs", |http, page| {
            let mut params = vec![("page", page.to_string())];
            if let Some(sha) = filter.sha.as_deref().filter(|value| !value.is_empty()) {
                params.push(("sha", sha.to_owned()));
            }
            if let Some(ref_name) = filter.ref_name.as_deref().filter(|value| !value.is_empty()) {
                params.push(("ref", ref_name.to_owned()));
            }
            if let Some(status) = filter.status.as_deref().filter(|value| !value.is_empty()) {
                params.push(("status", status.to_owned()));
            }
            http.get_page::<ApiPipeline>(&path, &params, "ci runs")
        })?;
        let runs: Vec<CiRunSummary> = pipelines.into_iter().map(Into::into).collect();
        Ok(CiRunsOutput {
            workflow_runs: runs,
            total_count: None,
            page: filter.page,
            limit: filter.limit,
        })
    }

    /// `GET /projects/:id/pipelines/:pipeline_id` for one pipeline.
    pub(crate) fn ci_run_get(&self, run_id: u64) -> Result<CiRunSummary, ForgejoError> {
        let pipeline: ApiPipeline =
            self.http
                .get(&self.pipeline_path(run_id), &[], "ci run get")?;
        Ok(pipeline.into())
    }

    /// `GET /projects/:id/pipelines/:pipeline_id/jobs` for the jobs
    /// attached to one pipeline. GitLab returns every job in a single
    /// page (no pagination); we still honour the same paginator
    /// envelope so the helper stays symmetrical with the runs list.
    pub(crate) fn ci_run_jobs(&self, run_id: u64) -> Result<CiJobsOutput, ForgejoError> {
        let path = self.pipeline_jobs_path(run_id);
        let jobs = self.http.paginate("ci run jobs", |http, page| {
            http.get_page::<ApiJob>(&path, &[("page", page.to_string())], "ci run jobs")
        })?;
        Ok(CiJobsOutput {
            run_id,
            jobs: jobs.into_iter().map(Into::into).collect(),
        })
    }

    /// `GET /projects/:id/jobs/:job_id/trace` returning the raw
    /// job trace. The orchestrator applies [`bound_log`] so the
    /// shared `CiJobLogsOutput` contract is preserved regardless of
    /// how the provider delivers the bytes.
    pub(crate) fn ci_job_logs(
        &self,
        job_id: u64,
        tail: usize,
    ) -> Result<CiJobLogsOutput, ForgejoError> {
        let raw = self
            .http
            .get_text(&self.job_trace_path(job_id), &[], "ci job logs")?;
        let (log, truncated, bytes) = bound_log(&raw, tail);
        Ok(CiJobLogsOutput {
            job_id,
            log,
            truncated,
            bytes,
        })
    }

    /// GitLab equivalent of [`crate::providers::forgejo::ci::ForgejoProvider::ci_inspect`].
    /// The shared `CiInspectOutput` contract is preserved; only the
    /// GitLab-specific status / conclusion mapping differs from the
    /// Forgejo implementation.
    pub(crate) fn ci_inspect(
        &self,
        request: &CiInspectRequest,
    ) -> Result<CiInspectOutput, ForgejoError> {
        let filter = CiRunsFilter {
            sha: Some(request.sha.clone()),
            ref_name: request.ref_name.clone(),
            status: None,
            workflow: None,
            page: 1,
            limit: 50,
        };
        let mut poll_count = 1_usize;
        let mut selected = self.select_ci_run(&filter)?;
        if selected.is_none() {
            if !request.wait {
                return Ok(self.inspect_output(
                    "no_run",
                    request,
                    None,
                    poll_count,
                    Vec::new(),
                    Vec::new(),
                ));
            }
            if request.timeout == 0 || request.poll == 0 {
                return Ok(self.inspect_output(
                    "timeout",
                    request,
                    None,
                    poll_count,
                    Vec::new(),
                    Vec::new(),
                ));
            }
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(request.timeout);
            while std::time::Instant::now() < deadline {
                std::thread::sleep(
                    std::time::Duration::from_secs(request.poll)
                        .min(deadline.saturating_duration_since(std::time::Instant::now())),
                );
                poll_count += 1;
                selected = self.select_ci_run(&filter)?;
                if selected.is_some() {
                    break;
                }
            }
            if selected.is_none() {
                return Ok(self.inspect_output(
                    "timeout",
                    request,
                    None,
                    poll_count,
                    Vec::new(),
                    Vec::new(),
                ));
            }
        }
        let mut state = run_state(selected.as_ref().expect("selected run exists"));
        if request.wait && state == "running" {
            if request.timeout == 0 || request.poll == 0 {
                state = "timeout";
            } else {
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(request.timeout);
                while state == "running" && std::time::Instant::now() < deadline {
                    std::thread::sleep(
                        std::time::Duration::from_secs(request.poll)
                            .min(deadline.saturating_duration_since(std::time::Instant::now())),
                    );
                    poll_count += 1;
                    let run_id = selected.as_ref().expect("selected run exists").id;
                    selected = Some(self.ci_run_get(run_id)?);
                    state = run_state(selected.as_ref().expect("selected run exists"));
                }
                if state == "running" {
                    state = "timeout";
                }
            }
        }
        let (failed_jobs, log_excerpts) = if state == "failure" {
            let jobs = selected
                .as_ref()
                .and_then(|run| self.ci_run_jobs(run.id).ok())
                .map(|jobs| jobs.jobs.into_iter().filter(job_failed).collect::<Vec<_>>())
                .unwrap_or_default();
            let excerpts = jobs
                .iter()
                .filter_map(|job| {
                    self.ci_job_logs(job.id, crate::ci_model::DEFAULT_LOG_TAIL)
                        .ok()
                        .map(|log| crate::ci_model::CiLogExcerpt {
                            job_id: job.id,
                            name: job.name.clone(),
                            log: log.log,
                            truncated: log.truncated,
                        })
                })
                .collect();
            (jobs, excerpts)
        } else {
            (Vec::new(), Vec::new())
        };
        Ok(self.inspect_output(
            state,
            request,
            selected,
            poll_count,
            failed_jobs,
            log_excerpts,
        ))
    }

    fn select_ci_run(&self, filter: &CiRunsFilter) -> Result<Option<CiRunSummary>, ForgejoError> {
        Ok(self
            .ci_runs(filter)?
            .workflow_runs
            .into_iter()
            .filter(|run| {
                let sha_matches = filter.sha.as_deref().is_none_or(|requested| {
                    run.commit_sha
                        .as_deref()
                        .or(run.head_sha.as_deref())
                        .is_none_or(|actual| actual == requested)
                });
                let ref_matches = filter.ref_name.as_deref().is_none_or(|requested| {
                    run.ref_name
                        .as_deref()
                        .or(run.pretty_ref.as_deref())
                        .is_none_or(|actual| {
                            actual == requested
                                || shared_pretty_ref(actual).as_deref() == Some(requested)
                        })
                });
                sha_matches && ref_matches
            })
            .max_by_key(|run| (run.run_number, run.id)))
    }

    fn inspect_output(
        &self,
        state: &str,
        request: &CiInspectRequest,
        selected: Option<CiRunSummary>,
        poll_count: usize,
        failed_jobs: Vec<crate::ci_model::CiJobSummary>,
        log_excerpts: Vec<crate::ci_model::CiLogExcerpt>,
    ) -> CiInspectOutput {
        let ref_name = request
            .ref_name
            .clone()
            .or_else(|| selected.as_ref().and_then(|run| run.ref_name.clone()));
        let url = selected.as_ref().and_then(|run| run.html_url.clone());
        CiInspectOutput {
            state: state.to_owned(),
            selected_run: selected,
            sha: request.sha.clone(),
            ref_name,
            url,
            failed_jobs,
            log_excerpts,
            checked_at: crate::ci_model::checked_at(),
            poll_count,
        }
    }
}

// -- Bridge helpers ---------------------------------------------------------

/// Some GitLab endpoints return the updated resource as the response
/// body, others return `200 OK` with no body. Decode the optional
/// body and surface a structured error when it is missing.
fn parse_optional_issue(
    option: Option<ApiIssue>,
    operation: &'static str,
) -> Result<ApiIssue, ForgejoError> {
    option.ok_or_else(|| {
        ForgejoError::not_found(operation, "GitLab did not return the updated issue")
    })
}

impl ApiIssue {
    fn into_summary(self, _provider: &GitlabProvider) -> IssueSummary {
        let state = state_from_gitlab(&self.state).to_owned();
        IssueSummary {
            id: self.id,
            number: self.iid,
            title: self.title,
            body: self.description,
            state,
            html_url: self.web_url,
        }
    }
}

/// Parsed result of [`GitlabProvider::resolve_namespace_target`].
/// Either a numeric namespace id (preferred for personal or explicit
/// group targets) or a string path is returned so the caller can
/// POST `/projects` with the right pair of fields.
#[derive(Debug)]
pub(crate) struct ResolvedNamespace {
    pub namespace_id: Option<u64>,
    pub path: String,
}

impl ApiProject {
    /// Convert the GitLab project payload into the shared
    /// `RepoSummary`. `full_name` is derived from the project's
    /// `path_with_namespace` when GitLab provides it; otherwise the
    /// namespace `path` and the project `path` are joined so the
    /// summary is still meaningful.
    fn into_summary(self) -> RepoSummary {
        let full_name = self
            .path_with_namespace
            .clone()
            .or_else(|| {
                self.namespace
                    .as_ref()
                    .and_then(|namespace| namespace.full_path.clone().or(namespace.path.clone()))
                    .map(|namespace_path| format!("{namespace_path}/{}", self.path))
            })
            .unwrap_or_else(|| self.path.clone());
        let owner = self
            .namespace
            .as_ref()
            .and_then(|namespace| namespace.full_path.clone().or(namespace.path.clone()))
            .or_else(|| {
                self.path_with_namespace
                    .as_ref()
                    .and_then(|value| value.rsplit_once('/').map(|(owner, _)| owner.to_owned()))
            })
            .unwrap_or_default();
        let private = matches!(
            self.visibility.as_deref(),
            Some("private") | Some("internal") | None
        ) && self.visibility.as_deref() != Some("public");
        RepoSummary {
            full_name,
            owner,
            name: self.path,
            private,
            clone_url: self.http_url_to_repo,
            ssh_url: self.ssh_url_to_repo,
            html_url: self.web_url,
        }
    }
}

impl From<ApiPipeline> for CiRunSummary {
    fn from(pipeline: ApiPipeline) -> Self {
        let status = pipeline_status_from_gitlab(&pipeline.status);
        let conclusion = pipeline_conclusion_from_gitlab(&pipeline.status, None);
        let ref_name = pipeline.ref_name.clone();
        let pretty_ref = ref_name.as_deref().and_then(shared_pretty_ref);
        Self {
            id: pipeline.id,
            run_number: pipeline.iid,
            status,
            conclusion,
            head_sha: pipeline.before_sha.clone(),
            commit_sha: pipeline.sha.clone(),
            ref_name,
            pretty_ref,
            workflow_id: None,
            html_url: pipeline.web_url,
            created: pipeline.created_at,
            started: pipeline.started_at,
            stopped: pipeline.finished_at,
        }
    }
}

impl From<ApiJob> for crate::ci_model::CiJobSummary {
    fn from(job: ApiJob) -> Self {
        let status = pipeline_status_from_gitlab(&job.status);
        let conclusion = pipeline_conclusion_from_gitlab(&job.status, job.conclusion.as_deref());
        let run_id = job.pipeline.as_ref().and_then(|pipeline| pipeline.id);
        Self {
            id: job.id,
            name: job.name,
            status,
            conclusion,
            run_id,
            attempt: job.queued_duration.map(|value| serde_json::json!(value)),
            task_id: None,
        }
    }
}

/// Map a `CiRunSummary` status / conclusion to the inspect state's
/// shared vocabulary. Mirrors the Forgejo inspector so the shared
/// CLI consumer does not need provider-specific branches.
///
/// GitLab exposes `skipped` as a terminal pipeline / job state that
/// is distinct from `failure`: a skipped run was deliberately
/// bypassed (for example by a `when: never` rule) and does not
/// represent a regression. The shared Forgejo inspector excludes
/// `skipped` from its failure set, so we mirror that exclusion here
/// and surface a dedicated `skipped` state.
fn run_state(run: &CiRunSummary) -> &'static str {
    let status = run.status.to_ascii_lowercase();
    let conclusion = run
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if status == "success" || conclusion == "success" {
        return "success";
    }
    if matches!(
        status.as_str(),
        "running" | "pending" | "waiting_for_resource" | "preparing" | "scheduled" | "created"
    ) {
        return "running";
    }
    if status == "skipped" || conclusion == "skipped" {
        return "skipped";
    }
    if matches!(
        status.as_str(),
        "failure" | "failed" | "error" | "cancelled" | "canceled" | "timed_out"
    ) || (!conclusion.is_empty() && conclusion != "success")
    {
        return "failure";
    }
    "unknown"
}

/// True when the job's terminal status indicates failure. Skipped
/// jobs are deliberately excluded: GitLab uses `skipped` for
/// `when: never` and `allow_failure: true` runs that did not
/// actually execute, and they must not block a CI-gated pipeline.
fn job_failed(job: &crate::ci_model::CiJobSummary) -> bool {
    let status = job.status.to_ascii_lowercase();
    let conclusion = job
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        status.as_str(),
        "failure" | "failed" | "error" | "cancelled" | "canceled" | "timed_out"
    ) || matches!(
        conclusion.as_str(),
        "failure" | "failed" | "error" | "cancelled" | "canceled" | "timed_out"
    )
}

impl ApiNote {
    /// Build a [`CommentOutput`] for the deserialised GitLab note.
    ///
    /// `issue_web_url` is the parent issue's `web_url` returned by
    /// GitLab (for example `https://gitlab.example/group/project/-/issues/7`).
    /// GitLab renders notes inline on the issue page, so the canonical
    /// note URL is `<issue_web_url>#note_<id>`. When the API did not
    /// surface a `web_url` for the parent issue, the function returns
    /// `None` rather than synthesising an `/api/v4` path that is not
    /// browsable from a web browser.
    fn into_output(self, issue_web_url: Option<&str>) -> CommentOutput {
        let html_url =
            issue_web_url.map(|base| format!("{}#note_{}", base.trim_end_matches('/'), self.id));
        CommentOutput {
            id: self.id,
            html_url,
            marker: None,
            body: Some(self.body.clone()),
        }
    }
}

impl CommentOutput {
    /// Replace the inferred marker with an explicit caller-provided
    /// marker. Used by `find_marker` so the returned `CommentOutput`
    /// always carries the marker that was searched for.
    fn with_marker(mut self, marker: String) -> Self {
        self.marker = Some(marker);
        self
    }
}

impl ApiIssueLink {
    /// Render the GitLab link as the orchestrator's shared
    /// [`RelationSummary`] shape, resolving the relation type from
    /// the queried issue's viewpoint.
    ///
    /// The link id is `issue_link_id` on `GET` responses and `id`
    /// on `POST` responses; the mapper prefers the explicit
    /// `issue_link_id` field and falls back to `id` so both
    /// contract fixtures and the live shapes decode to the same
    /// id. The queried issue's iid becomes `issue_id`. The
    /// linked issue's iid is read from `target_issue` (POST
    /// response), `issue` (legacy GET fixtures), or the top-level
    /// `iid` (live GET response) in that order so the flat live
    /// shape and the legacy nested shape both surface the right
    /// `issue_to_id`. `delay` is always `None` for GitLab because
    /// the API has no notion of a precedence lag.
    fn into_summary(self, queried_issue_iid: u64) -> RelationSummary {
        let link_id = self.issue_link_id.or(self.id).unwrap_or(0);
        let linked_iid = self
            .target_issue
            .as_ref()
            .map(|endpoint| endpoint.iid)
            .or_else(|| self.issue.as_ref().map(|issue| issue.iid))
            .or(self.iid)
            .unwrap_or(0);
        let relation_type = if self.link_type.is_empty() {
            // GitLab 19.x always reports a `link_type`; an empty
            // value surfaces as `unknown` so an operator can spot
            // the regression rather than seeing a silent `relates`
            // default.
            "unknown".to_owned()
        } else {
            crate::providers::gitlab::model::gitlab_link_type_to_relation_type(&self.link_type)
                .as_str()
                .to_owned()
        };
        RelationSummary {
            id: link_id,
            relation_type,
            issue_id: queried_issue_iid,
            issue_to_id: linked_iid,
            delay: None,
        }
    }
}

fn label_color(name: &str) -> &'static str {
    // GitLab requires a valid hex color for new labels. The exact
    // shade does not matter for the orchestrator workflow, so each
    // managed label gets a stable, distinguishable color that does
    // not collide with the GitLab default palette.
    match name {
        "workflow::new" => "#1f75cb",
        "workflow::in-progress" => "#e6a23c",
        "workflow::in-review" => "#8e44ad",
        "workflow::changes-requested" => "#d63a3a",
        "workflow::blocked" => "#6c757d",
        "workflow::resolved" => "#28a745",
        "workflow::closed" => "#222222",
        "workflow::cancelled" => "#bf2e2e",
        TRACKER_LABEL_BUG => "#d63a3a",
        TRACKER_LABEL_FEATURE => "#28a745",
        other => {
            // Stable hash-based fallback for any future label that
            // does not get an explicit assignment above.
            let bytes = other.as_bytes();
            let hash = bytes.iter().fold(0u32, |acc, byte| {
                (acc.wrapping_mul(31)).wrapping_add(*byte as u32)
            });
            let r = (hash & 0xff) as u8;
            let g = ((hash >> 8) & 0xff) as u8;
            let b = ((hash >> 16) & 0xff) as u8;
            let hex = format!("#{r:02x}{g:02x}{b:02x}");
            Box::leak(hex.into_boxed_str())
        }
    }
}

// -- Capability surface ----------------------------------------------------

impl GitlabProvider {
    /// Capability table for GitLab. Phase 3 lights up repository
    /// creation and CI read paths alongside the Phase 2 issue /
    /// comment / workflow surface. Phase 4 lifts the relation surface
    /// from not-supported to native so the shared CLI can dispatch
    /// `relation list/create/delete` to the GitLab provider.
    pub(crate) fn capabilities(&self) -> crate::providers::ProviderCapabilities {
        crate::providers::ProviderCapabilities {
            issue_lifecycle: true,
            comments: true,
            repository_creation: true,
        }
    }

    pub(crate) fn supports(&self, capability: Capability) -> bool {
        match capability {
            Capability::IssueRead
            | Capability::IssueSearch
            | Capability::IssueCreate
            | Capability::IssueUpdateBody
            | Capability::IssueClose => true,
            Capability::CommentCreate | Capability::CommentRead | Capability::CommentFindMarker => {
                true
            }
            Capability::RepoCreate => true,
            Capability::CiRead => true,
            Capability::RelationRead | Capability::RelationCreate | Capability::RelationDelete => {
                true
            }
            Capability::ProjectRead
            | Capability::ProjectCreate
            | Capability::IssueStatusRead
            | Capability::VersionRead => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_color_is_a_valid_hex_string_for_every_managed_label() {
        let labels = [
            "workflow::new",
            "workflow::in-progress",
            "workflow::in-review",
            "workflow::changes-requested",
            "workflow::blocked",
            "workflow::resolved",
            "workflow::closed",
            "workflow::cancelled",
            "type::bug",
            "type::feature",
        ];
        for label in labels {
            let color = label_color(label);
            assert!(
                color.starts_with('#') && color.len() == 7,
                "{label} produced invalid color {color}"
            );
            assert!(
                color[1..].chars().all(|c| c.is_ascii_hexdigit()),
                "{label} produced non-hex color {color}"
            );
        }
    }

    #[test]
    fn label_color_fallback_is_deterministic_for_unknown_names() {
        let first = label_color("custom::thing");
        let second = label_color("custom::thing");
        assert_eq!(first, second);
    }

    #[test]
    fn capabilities_match_phase_4_scope() {
        let provider = GitlabProvider::new(
            GitlabConfig::new("https://gitlab.example/api/v4", 42),
            "test-token".to_owned(),
        )
        .unwrap();
        let caps = provider.capabilities();
        assert!(caps.issue_lifecycle);
        assert!(caps.comments);
        assert!(caps.repository_creation);
        assert!(provider.supports(Capability::IssueRead));
        assert!(provider.supports(Capability::CommentCreate));
        assert!(provider.supports(Capability::RepoCreate));
        assert!(provider.supports(Capability::CiRead));
        // Phase 4: relations are native on GitLab.
        assert!(provider.supports(Capability::RelationRead));
        assert!(provider.supports(Capability::RelationCreate));
        assert!(provider.supports(Capability::RelationDelete));
        assert!(!provider.supports(Capability::IssueStatusRead));
        assert!(!provider.supports(Capability::ProjectRead));
    }

    #[test]
    fn resolve_namespace_target_personal_path() {
        let resolved = GitlabProvider::resolve_namespace_target("widget", None, 7).unwrap();
        assert_eq!(resolved.path, "widget");
        assert_eq!(resolved.namespace_id, Some(7));
    }

    #[test]
    fn resolve_namespace_target_owner_repo_keeps_owner_without_explicit_id() {
        let resolved = GitlabProvider::resolve_namespace_target("acme/widgets", None, 7).unwrap();
        assert_eq!(resolved.path, "widgets");
        // Owner supplied without an explicit id: leave it None so a
        // caller that wants strict namespace resolution must ask for it.
        assert_eq!(resolved.namespace_id, None);
    }

    #[test]
    fn resolve_namespace_target_owner_repo_with_explicit_id() {
        let resolved =
            GitlabProvider::resolve_namespace_target("acme/widgets", Some(99), 7).unwrap();
        assert_eq!(resolved.path, "widgets");
        assert_eq!(resolved.namespace_id, Some(99));
    }

    #[test]
    fn resolve_namespace_target_empty_string_errors() {
        let error = GitlabProvider::resolve_namespace_target("", None, 7).unwrap_err();
        assert!(error.to_string().contains("non-empty"));
    }

    #[test]
    fn pipeline_status_from_gitlab_maps_known_states() {
        assert_eq!(pipeline_status_from_gitlab("running"), "running");
        assert_eq!(pipeline_status_from_gitlab("success"), "success");
        assert_eq!(pipeline_status_from_gitlab("failed"), "failure");
        assert_eq!(pipeline_status_from_gitlab("canceled"), "cancelled");
        assert_eq!(pipeline_status_from_gitlab("cancelled"), "cancelled");
        assert_eq!(pipeline_status_from_gitlab("skipped"), "skipped");
        assert_eq!(pipeline_status_from_gitlab("manual"), "manual");
        assert_eq!(pipeline_status_from_gitlab("pending"), "pending");
        assert_eq!(pipeline_status_from_gitlab("created"), "pending");
        assert_eq!(
            pipeline_status_from_gitlab("waiting_for_resource"),
            "pending"
        );
        assert_eq!(pipeline_status_from_gitlab("preparing"), "pending");
        assert_eq!(pipeline_status_from_gitlab("scheduled"), "pending");
    }

    #[test]
    fn pipeline_status_from_gitlab_preserves_unknown_states() {
        // Unknown GitLab statuses must surface unchanged so an
        // operator can spot them rather than seeing a silent
        // remapping to "unknown".
        assert_eq!(
            pipeline_status_from_gitlab("brand-new-state"),
            "brand-new-state"
        );
    }

    #[test]
    fn pipeline_conclusion_uses_status_for_terminal_states() {
        assert_eq!(
            pipeline_conclusion_from_gitlab("success", Some("ignored")),
            Some("success".to_owned())
        );
        assert_eq!(
            pipeline_conclusion_from_gitlab("failed", Some("ignored")),
            Some("failed".to_owned())
        );
        assert_eq!(
            pipeline_conclusion_from_gitlab("cancelled", None),
            Some("cancelled".to_owned())
        );
        assert_eq!(
            pipeline_conclusion_from_gitlab("running", Some("ignored")),
            None
        );
        assert_eq!(
            pipeline_conclusion_from_gitlab("pending", Some("ignored")),
            None
        );
    }
}
