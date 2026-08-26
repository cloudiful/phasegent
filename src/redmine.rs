use crate::forgejo_model::{CommentOutput, ForgejoError, IssueSummary};
use crate::policy::Capability;
use crate::provider_config::RedmineProvider;
use crate::redmine_model::{
    IssuePlanning, RedmineBootstrap, RedmineCurrentUser, RedmineGitMirrorOutcome,
    RedmineGitMirrorRequest, RedmineGitMirrorResponse, RedmineIssue, RedmineIssueCollection,
    RedmineIssueResponse, RedmineIssueStatus, RedmineIssueStatusCollection, RedmineNewIssue,
    RedmineNewProject, RedmineNewRelation, RedmineNewTimeEntry, RedmineNotes, RedmineProject,
    RedmineProjectCollection, RedmineProjectResponse, RedmineRelationCollection,
    RedmineRelationResponse, RedmineRelationType, RedmineTimeEntry, RedmineTimeEntryActivity,
    RedmineTimeEntryActivityCollection, RedmineTimeEntryCollection, RedmineTimeEntryResponse,
    RedmineTracker, RedmineTrackerCollection, RedmineUpdateIssue, RedmineUserMembershipOutcome,
    RedmineVersion, RedmineVersionCollection, RelationSummary,
};

const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 10_000;

impl RedmineProvider {
    pub fn bootstrap_project(
        &self,
        repository: &str,
        identifier: &str,
        close_status_id: Option<&str>,
        close_status_name: Option<&str>,
    ) -> Result<RedmineBootstrap, ForgejoError> {
        let project = self.find_project(identifier)?;
        let statuses = self.list_issue_statuses()?;
        let close_status =
            Self::select_close_status(&statuses, close_status_id, close_status_name)?.clone();
        let created = project.is_none();
        let project = match project {
            Some(project) => project,
            None => self.create_project(
                repository,
                identifier,
                Some(&format!("Workflow issues for {repository}")),
            )?,
        };
        Ok(RedmineBootstrap {
            project,
            close_status,
            created,
        })
    }

    /// Identify the user bound to this provider's API key via
    /// `/users/current.json`. Bootstrap uses this to map a role-scoped
    /// credential to a concrete Redmine user without a separate login flow.
    pub fn current_user(&self) -> Result<RedmineCurrentUser, ForgejoError> {
        self.http.current_user()
    }

    /// Ensure the given user holds `role_name` on the project, adding the
    /// membership or appending the role without dropping any unrelated
    /// roles. Bootstrap reconciles one such outcome per agent identity.
    pub fn ensure_user_membership(
        &self,
        project_id: u64,
        user: &RedmineCurrentUser,
        role_name: &str,
    ) -> Result<RedmineUserMembershipOutcome, ForgejoError> {
        self.http
            .ensure_user_membership(project_id, user, role_name)
    }

    pub fn list_projects(&self) -> Result<Vec<RedmineProject>, ForgejoError> {
        let mut projects = Vec::new();
        let mut offset: usize = 0;
        let mut previous_signature = None;
        for _ in 0..MAX_PAGES {
            let params = [
                ("limit", PAGE_SIZE.to_string()),
                ("offset", offset.to_string()),
            ];
            let page: RedmineProjectCollection =
                self.http.get("projects.json", &params, "project list")?;
            let signature = page
                .projects
                .iter()
                .map(|project| project.id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            if previous_signature.as_deref() == Some(signature.as_str())
                && !page.projects.is_empty()
            {
                return Err(ForgejoError::pagination(
                    "project list",
                    "Redmine returned the same non-empty page repeatedly",
                ));
            }

            let count = page.projects.len();
            let response_limit = page.limit.unwrap_or(PAGE_SIZE).max(1);
            let total_count = page.total_count;
            projects.extend(page.projects);
            let complete = count == 0
                || total_count.is_some_and(|total| offset.saturating_add(count) >= total)
                || (total_count.is_none() && count < response_limit);
            if complete {
                return Ok(projects);
            }
            let next_offset = offset.saturating_add(count);
            if next_offset <= offset {
                return Err(ForgejoError::pagination(
                    "project list",
                    "Redmine pagination offset did not advance",
                ));
            }
            offset = next_offset;
            previous_signature = Some(signature);
        }
        Err(ForgejoError::pagination(
            "project list",
            "pagination exceeded the safety limit",
        ))
    }

    pub fn create_project(
        &self,
        name: &str,
        identifier: &str,
        description: Option<&str>,
    ) -> Result<RedmineProject, ForgejoError> {
        if name.trim().is_empty() {
            return Err(ForgejoError::config("project name cannot be empty"));
        }
        if identifier.trim().is_empty() {
            return Err(ForgejoError::config("project identifier cannot be empty"));
        }
        let payload =
            RedmineNewProject::new(name, identifier, description).with_repository_module();
        let response: RedmineProjectResponse =
            self.http
                .post("projects.json", &payload, "project create")?;
        Ok(response.project)
    }

    pub fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, ForgejoError> {
        let response: RedmineIssueStatusCollection =
            self.http
                .get("issue_statuses.json", &[], "issue status list")?;
        Ok(response.issue_statuses)
    }

    /// List every tracker visible to this API key (`/trackers.json`). The
    /// configured workflow only uses Bug and Feature, but resolution stays
    /// generic so the server remains the source of truth.
    pub fn list_trackers(&self) -> Result<Vec<RedmineTracker>, ForgejoError> {
        let response: RedmineTrackerCollection =
            self.http.get("trackers.json", &[], "tracker list")?;
        Ok(response.trackers)
    }

    /// List time-entry activities from Redmine's enumeration endpoint.
    pub fn list_time_entry_activities(
        &self,
    ) -> Result<Vec<RedmineTimeEntryActivity>, ForgejoError> {
        let response: RedmineTimeEntryActivityCollection = self.http.get(
            "enumerations/time_entry_activities.json",
            &[],
            "time entry activity list",
        )?;
        Ok(response.time_entry_activities)
    }

    /// Create one issue Time Entry. A 201 response may contain the created
    /// object; 204 or an empty 201 response is represented by `Ok(None)`.
    pub fn create_time_entry(
        &self,
        issue_id: u64,
        hours: f64,
        spent_on: &str,
        activity_id: u64,
        comments: &str,
    ) -> Result<Option<RedmineTimeEntry>, ForgejoError> {
        if issue_id == 0 {
            return Err(ForgejoError::config(
                "Redmine time entry issue id must be greater than zero",
            ));
        }
        if activity_id == 0 {
            return Err(ForgejoError::config(
                "Redmine time entry activity id must be greater than zero",
            ));
        }
        if !hours.is_finite() || hours <= 0.0 {
            return Err(ForgejoError::config(
                "Redmine time entry hours must be positive",
            ));
        }
        if spent_on.trim().is_empty() {
            return Err(ForgejoError::config(
                "Redmine time entry spent_on date cannot be empty",
            ));
        }
        if comments.trim().is_empty() {
            return Err(ForgejoError::config(
                "Redmine time entry comments cannot be empty",
            ));
        }
        let payload = RedmineNewTimeEntry {
            time_entry: crate::redmine_model::RedmineNewTimeEntryFields {
                issue_id,
                hours,
                spent_on,
                activity_id,
                comments,
            },
        };
        let response: Option<RedmineTimeEntryResponse> =
            self.http
                .post_optional("time_entries.json", &payload, "time entry create")?;
        Ok(response
            .and_then(|response| response.time_entry)
            .filter(|entry| entry.id != 0))
    }

    /// List Time Entries visible to the current API key, optionally bounded
    /// by issue and date. Pagination uses the bounded safeguards shared by
    /// the other Redmine list operations.
    pub fn list_time_entries(
        &self,
        issue_id: u64,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<RedmineTimeEntry>, ForgejoError> {
        if issue_id == 0 {
            return Err(ForgejoError::config(
                "Redmine time entry issue id must be greater than zero",
            ));
        }
        let from = from.map(str::to_owned);
        let to = to.map(str::to_owned);
        self.http.paginate("time entry list", |http, offset| {
            let mut params = vec![
                ("issue_id", issue_id.to_string()),
                ("limit", PAGE_SIZE.to_string()),
                ("offset", offset.to_string()),
            ];
            if let Some(from) = &from {
                params.push(("from", from.clone()));
            }
            if let Some(to) = &to {
                params.push(("to", to.clone()));
            }
            let page: RedmineTimeEntryCollection =
                http.get("time_entries.json", &params, "time entry list")?;
            let signature = page
                .time_entries
                .iter()
                .map(|entry| entry.id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            Ok((page.time_entries, page.total_count, page.limit, signature))
        })
    }

    /// Find a marker-matching Time Entry before retrying a projection. This
    /// closes the 204/empty-response window and also recovers a request that
    /// succeeded remotely but lost its response locally.
    pub fn find_time_entry_by_comments(
        &self,
        issue_id: u64,
        spent_on: &str,
        comments: &str,
    ) -> Result<Option<RedmineTimeEntry>, ForgejoError> {
        if comments.trim().is_empty() {
            return Err(ForgejoError::config(
                "Redmine time entry marker cannot be empty",
            ));
        }
        let entries = self.list_time_entries(issue_id, Some(spent_on), Some(spent_on))?;
        let mut matches = entries
            .into_iter()
            .filter(|entry| {
                entry.comments.as_deref() == Some(comments)
                    && entry
                        .issue
                        .as_ref()
                        .is_none_or(|issue| issue.id == issue_id)
                    && entry
                        .spent_on
                        .as_deref()
                        .is_none_or(|entry_date| entry_date == spent_on)
            })
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(ForgejoError::config(format!(
                "multiple Redmine Time Entries match run marker '{}'",
                comments
            )));
        }
        Ok(matches.pop())
    }

    /// List the versions of the configured project
    /// (`/projects/:id/versions.json`), including shared versions Redmine
    /// makes visible for planning. Results are paginated across all pages
    /// with repeated-page and non-advancing-offset safeguards so large
    /// roadmaps never silently miss versions during `--fixed-version`
    /// resolution. Version discovery is project-scoped, so a configured
    /// project id is required.
    pub fn list_versions(&self) -> Result<Vec<RedmineVersion>, ForgejoError> {
        let project_id = self.config.require_project_id()?;
        let path = format!("projects/{project_id}/versions.json");
        let mut versions = Vec::new();
        let mut offset: usize = 0;
        let mut previous_signature = None;
        for _ in 0..MAX_PAGES {
            let params = [
                ("limit", PAGE_SIZE.to_string()),
                ("offset", offset.to_string()),
            ];
            let page: RedmineVersionCollection = self.http.get(&path, &params, "version list")?;
            let signature = page
                .versions
                .iter()
                .map(|version| version.id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            if previous_signature.as_deref() == Some(signature.as_str())
                && !page.versions.is_empty()
            {
                return Err(ForgejoError::pagination(
                    "version list",
                    "Redmine returned the same non-empty page repeatedly",
                ));
            }

            let count = page.versions.len();
            let response_limit = page.limit.unwrap_or(PAGE_SIZE).max(1);
            let total_count = page.total_count;
            versions.extend(page.versions);
            let complete = count == 0
                || total_count.is_some_and(|total| offset.saturating_add(count) >= total)
                || (total_count.is_none() && count < response_limit);
            if complete {
                return Ok(versions);
            }
            let next_offset = offset.saturating_add(count);
            if next_offset <= offset {
                return Err(ForgejoError::pagination(
                    "version list",
                    "Redmine pagination offset did not advance",
                ));
            }
            offset = next_offset;
            previous_signature = Some(signature);
        }
        Err(ForgejoError::pagination(
            "version list",
            "pagination exceeded the safety limit",
        ))
    }

    pub fn get_issue(&self, number: u64) -> Result<IssueSummary, ForgejoError> {
        let issue = self.issue_with_journals(number, "issue get")?;
        Ok(self.issue_summary(issue))
    }

    pub fn search_issues(
        &self,
        query: Option<&str>,
        state: &str,
    ) -> Result<Vec<IssueSummary>, ForgejoError> {
        let project_id = self
            .config
            .project_id
            .as_deref()
            .filter(|project_id| !project_id.trim().is_empty())
            .map(str::to_owned);
        let status_id = match state {
            "open" => "open",
            "closed" => "closed",
            "all" => "*",
            _ => {
                return Err(ForgejoError::config(
                    "issue state must be open, closed, or all",
                ));
            }
        };

        let mut issues = Vec::new();
        let mut offset: usize = 0;
        let mut previous_signature = None;
        for _ in 0..MAX_PAGES {
            let mut params = vec![
                ("status_id", status_id.to_owned()),
                ("limit", PAGE_SIZE.to_string()),
                ("offset", offset.to_string()),
            ];
            if let Some(project_id) = &project_id {
                params.push(("project_id", project_id.clone()));
            }
            if let Some(query) = query.filter(|query| !query.is_empty()) {
                params.push(("subject", format!("~{query}")));
            }
            let page: RedmineIssueCollection =
                self.http.get("issues.json", &params, "issue search")?;
            let signature = page.signature();
            if previous_signature.as_deref() == Some(signature.as_str()) && !page.issues.is_empty()
            {
                return Err(ForgejoError::pagination(
                    "issue search",
                    "Redmine returned the same non-empty page repeatedly",
                ));
            }

            let count = page.issues.len();
            let response_limit = page.limit.unwrap_or(PAGE_SIZE).max(1);
            let total_count = page.total_count;
            issues.extend(
                page.issues
                    .into_iter()
                    .filter(|issue| issue.matches_state(state))
                    .map(|issue| self.issue_summary(issue)),
            );

            let complete = count == 0
                || total_count.is_some_and(|total| offset.saturating_add(count) >= total)
                || (total_count.is_none() && count < response_limit);
            if complete {
                return Ok(issues);
            }
            let next_offset = offset.saturating_add(count);
            if next_offset <= offset {
                return Err(ForgejoError::pagination(
                    "issue search",
                    "Redmine pagination offset did not advance",
                ));
            }
            offset = next_offset;
            previous_signature = Some(signature);
        }
        Err(ForgejoError::pagination(
            "issue search",
            "pagination exceeded the safety limit",
        ))
    }

    pub fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, ForgejoError> {
        self.create_issue_with_planning(title, body, None, &IssuePlanning::default())
    }

    /// Create an issue with an explicit tracker id already resolved through
    /// [`RedmineProvider::select_tracker`].
    pub fn create_issue_with_tracker(
        &self,
        title: &str,
        body: &str,
        tracker_id: u64,
    ) -> Result<IssueSummary, ForgejoError> {
        self.create_issue_with_planning(title, body, Some(tracker_id), &IssuePlanning::default())
    }

    /// Create an issue with an optional tracker plus native planning
    /// fields. Fields absent from `planning` stay out of the JSON payload
    /// so the legacy create request shape is preserved.
    pub fn create_issue_with_planning(
        &self,
        title: &str,
        body: &str,
        tracker_id: Option<u64>,
        planning: &IssuePlanning,
    ) -> Result<IssueSummary, ForgejoError> {
        let project_id = self.config.require_project_id()?;
        let payload = RedmineNewIssue::new(project_id, title, body)
            .with_tracker_option(tracker_id)
            .with_planning(planning);
        let response: RedmineIssueResponse =
            self.http.post("issues.json", &payload, "issue create")?;
        Ok(self.issue_summary(response.issue))
    }

    pub fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, ForgejoError> {
        let payload = RedmineUpdateIssue::description(body);
        self.put_issue_update(number, payload, "issue update-body")
    }

    /// Update the body and re-target the tracker in a single PUT. The
    /// tracker id must already be resolved through
    /// [`RedmineProvider::select_tracker`].
    pub fn update_body_with_tracker(
        &self,
        number: u64,
        body: &str,
        tracker_id: u64,
    ) -> Result<IssueSummary, ForgejoError> {
        let payload = RedmineUpdateIssue::description_with_tracker(body, tracker_id);
        self.put_issue_update(number, payload, "issue update-body")
    }

    /// Update the body with an optional tracker re-target plus native
    /// planning fields in one atomic PUT. Fields absent from `planning`
    /// stay out of the JSON payload so the legacy update request shape is
    /// preserved.
    pub fn update_body_with_planning(
        &self,
        number: u64,
        body: &str,
        tracker_id: Option<u64>,
        planning: &IssuePlanning,
    ) -> Result<IssueSummary, ForgejoError> {
        let payload = match tracker_id {
            Some(tracker_id) => RedmineUpdateIssue::description_with_tracker(body, tracker_id),
            None => RedmineUpdateIssue::description(body),
        }
        .with_planning(planning);
        self.put_issue_update(number, payload, "issue update-body")
    }

    fn put_issue_update(
        &self,
        number: u64,
        payload: RedmineUpdateIssue<'_>,
        operation: &'static str,
    ) -> Result<IssueSummary, ForgejoError> {
        let response: Option<RedmineIssueResponse> =
            self.http
                .put(&self.issue_path(number), &payload, operation)?;
        response
            .map(|response| self.issue_summary(response.issue))
            .map_or_else(|| self.get_issue(number), Ok)
    }

    /// Move an issue to any status resolved by validated name or id via
    /// [`RedmineProvider::select_status_by_value`]. Unlike `close_issue`
    /// this is not restricted to closed statuses.
    pub fn set_issue_status(
        &self,
        number: u64,
        status_id: u64,
    ) -> Result<IssueSummary, ForgejoError> {
        let payload = RedmineUpdateIssue::status(status_id);
        let response: Option<RedmineIssueResponse> =
            self.http
                .put(&self.issue_path(number), &payload, "issue status update")?;
        response
            .map(|response| self.issue_summary(response.issue))
            .map_or_else(|| self.get_issue(number), Ok)
    }

    pub fn close_issue(&self, number: u64) -> Result<IssueSummary, ForgejoError> {
        let status_id = self.config.require_close_status_id()?;
        let payload = RedmineUpdateIssue::status(status_id);
        let response: Option<RedmineIssueResponse> =
            self.http
                .put(&self.issue_path(number), &payload, "issue close")?;
        response
            .map(|response| self.issue_summary(response.issue))
            .map_or_else(|| self.get_issue(number), Ok)
    }

    pub fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, ForgejoError> {
        if marker.is_empty() {
            return Err(ForgejoError::config("marker cannot be empty"));
        }
        let payload = RedmineNotes {
            issue: crate::redmine_model::RedmineNotesFields { notes: body },
        };
        let response: Option<RedmineIssueResponse> =
            self.http
                .put(&self.issue_path(issue), &payload, "comment create")?;
        let issue = match response {
            Some(response) if response.issue.find_journal(body, marker).is_some() => response.issue,
            _ => self.issue_with_journals(issue, "comment create")?,
        };
        let journal = issue.find_journal(body, marker).ok_or_else(|| {
            ForgejoError::not_found(
                "comment create",
                "Redmine did not return the created journal",
            )
        })?;
        Ok(journal.to_comment(&self.http.issue_url(issue.id), Some(marker), false))
    }

    pub fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, ForgejoError> {
        let issue_data = self.issue_with_journals(issue, "comment get")?;
        let journal = issue_data
            .journals
            .iter()
            .find(|journal| journal.id == comment)
            .ok_or_else(|| {
                ForgejoError::not_found(
                    "comment get",
                    "comment was not found in the specified issue",
                )
            })?;
        Ok(journal.to_comment(&self.http.issue_url(issue_data.id), None, true))
    }

    pub fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, ForgejoError> {
        if marker.is_empty() {
            return Err(ForgejoError::config("marker cannot be empty"));
        }
        let issue_data = self.issue_with_journals(issue, "comment find-marker")?;
        let journal = issue_data
            .journals
            .iter()
            .find(|journal| journal.notes.contains(marker))
            .ok_or_else(|| {
                ForgejoError::not_found("comment find-marker", "marker was not found")
            })?;
        Ok(journal.to_comment(&self.http.issue_url(issue_data.id), Some(marker), false))
    }

    fn issue_with_journals(
        &self,
        number: u64,
        operation: &str,
    ) -> Result<RedmineIssue, ForgejoError> {
        let params = [("include", "journals".to_owned())];
        let response: RedmineIssueResponse =
            self.http
                .get(&self.issue_path(number), &params, operation)?;
        Ok(response.issue)
    }

    fn issue_path(&self, number: u64) -> String {
        format!("issues/{number}.json")
    }

    fn issue_summary(&self, issue: RedmineIssue) -> IssueSummary {
        let url = self.http.issue_url(issue.id);
        issue.into_summary(url)
    }

    /// List the relations of a single issue
    /// (`/issues/:id/relations.json`). Each relation is rendered from the
    /// queried issue's viewpoint so inverse names appear correctly.
    pub fn list_relations(&self, issue: u64) -> Result<Vec<RelationSummary>, ForgejoError> {
        let path = format!("issues/{issue}/relations.json");
        let collection: RedmineRelationCollection = self.http.get(&path, &[], "relation list")?;
        Ok(collection
            .relations
            .into_iter()
            .map(|relation| relation.into_summary(issue))
            .collect())
    }

    /// Create a relation from `issue` to `to` with a canonical `--type`.
    /// `delay` is only meaningful for `precedes` and is omitted otherwise.
    /// Returns the created relation, matching the shared provider create
    /// shape.
    pub fn create_relation(
        &self,
        issue: u64,
        to: u64,
        relation_type: RedmineRelationType,
        delay: Option<u64>,
    ) -> Result<RelationSummary, ForgejoError> {
        let path = format!("issues/{issue}/relations.json");
        let payload = RedmineNewRelation::new(to, relation_type.as_str(), delay);
        let response: RedmineRelationResponse =
            self.http.post(&path, &payload, "relation create")?;
        Ok(response.relation.into_summary(issue))
    }

    /// Delete a relation by its numeric id (`DELETE /relations/:id.json`).
    /// Mirrors the shared provider shape: a successful delete returns no body.
    pub fn delete_relation(&self, relation_id: u64) -> Result<(), ForgejoError> {
        let path = format!("relations/{relation_id}.json");
        self.http
            .delete::<serde_json::Value>(&path, "relation delete")
            .map(|_| ())
    }
}

impl RedmineProvider {
    pub(crate) fn capabilities(&self) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            issue_lifecycle: true,
            comments: true,
            repository_creation: false,
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
            Capability::ProjectRead | Capability::ProjectCreate | Capability::IssueStatusRead => {
                true
            }
            Capability::VersionRead => true,
            Capability::RelationRead | Capability::RelationCreate | Capability::RelationDelete => {
                true
            }
            Capability::RepoCreate | Capability::CiRead => false,
        }
    }
}

/// Register the current repository's Git URL with the `redmine_git_mirror`
/// plugin, returning the resulting `RedmineGitMirrorOutcome`.
///
/// The helper expects the Redmine **base URL** (no `/api/v1` suffix) plus a
/// deployment-level bearer key read from
/// `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`. The plugin's canonical mirror
/// identifier is `mirror_<project_id>_<owner>_<repo>` (lowercased); we
/// `GET` it first so a previously-queued or already-ready mirror is
/// reported as `existing`/`pending`/`cloning`/`ready` without a duplicate
/// POST. A missing entry triggers a `POST
/// /sys/redmine_git_mirror/projects/<id>/repository`, and an entry whose
/// plugin-reported status is `failed` triggers the same POST once to
/// requeue the mirror (a failed job never converges on its own); the
/// queued URL always comes from this function's arguments, never from the
/// plugin response.
pub fn register_git_mirror(
    redmine_base_url: &str,
    project_id: u64,
    owner: &str,
    repo: &str,
    remote_url: &str,
) -> Result<RedmineGitMirrorOutcome, ForgejoError> {
    if project_id == 0 {
        return Err(ForgejoError::config(
            "Redmine project id must be greater than zero to register a git mirror",
        ));
    }
    if remote_url.trim().is_empty() {
        return Err(ForgejoError::config(
            "git mirror remote URL must not be empty",
        ));
    }
    let bearer_key = crate::auth::redmine_git_mirror_api_key().map_err(ForgejoError::config)?;
    let Some(bearer_key) = bearer_key else {
        return Err(ForgejoError::config(
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY is not set; \
             set the Redmine git mirror plugin key in the environment to queue mirrors",
        ));
    };
    let http =
        crate::redmine_http::RedmineGitMirrorHttp::new(redmine_base_url.to_owned(), bearer_key)?;
    let identifier = mirror_identifier(project_id, owner, repo);
    let get_path = format!("/sys/redmine_git_mirror/projects/{project_id}/repository/{identifier}");
    let post_path = format!("/sys/redmine_git_mirror/projects/{project_id}/repository");
    let response = match http.get::<RedmineGitMirrorResponse>(&get_path, "mirror get")? {
        crate::redmine_http::RedmineGitMirrorLookup::Found(response) => {
            // Only a `failed` existing mirror is requeued; `pending`,
            // `cloning`, and `ready` stay idempotent with a single GET.
            if response.status.trim().eq_ignore_ascii_case("failed") {
                let body = RedmineGitMirrorRequest::new(remote_url);
                http.post::<RedmineGitMirrorResponse, _>(&post_path, &body, "mirror post")?
            } else {
                response
            }
        }
        crate::redmine_http::RedmineGitMirrorLookup::Missing => {
            let body = RedmineGitMirrorRequest::new(remote_url);
            http.post::<RedmineGitMirrorResponse, _>(&post_path, &body, "mirror post")?
        }
    };
    outcome_from_response(response)
}

pub(crate) fn mirror_identifier(project_id: u64, owner: &str, repo: &str) -> String {
    let owner = owner.trim().to_ascii_lowercase();
    let repo = repo.trim().to_ascii_lowercase();
    format!("mirror_{project_id}_{owner}_{repo}")
}

fn outcome_from_response(
    response: RedmineGitMirrorResponse,
) -> Result<RedmineGitMirrorOutcome, ForgejoError> {
    let status = response.status.trim().to_ascii_lowercase();
    if status == "failed" {
        let detail = response
            .error
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "the plugin reported a failed mirror status".to_owned());
        return Err(ForgejoError::config(format!(
            "Redmine git mirror plugin reported a failed status for {}: {}",
            response.identifier, detail
        )));
    }
    Ok(RedmineGitMirrorOutcome {
        id: response.id,
        project_id: response.project_id,
        identifier: response.identifier,
        status: normalise_status(response.status.as_str()),
        remote_url: response.remote_url.unwrap_or_default(),
        local_path: response.local_path.unwrap_or_default(),
        error: response.error,
    })
}

fn normalise_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pending" | "cloning" | "ready" | "failed" => raw.trim().to_ascii_lowercase(),
        other => other.to_owned(),
    }
}
