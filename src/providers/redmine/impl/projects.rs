use crate::providers::api::ForgejoError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::RedmineNewProject;
use crate::providers::redmine::model::{
    RedmineBootstrap, RedmineCurrentUser, RedmineIssueStatus, RedmineProject,
    RedmineProjectCollection, RedmineProjectResponse, RedmineUserMembershipOutcome, RedmineVersion,
    RedmineVersionCollection,
};

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
        for _ in 0..super::MAX_PAGES {
            let params = [
                ("limit", super::PAGE_SIZE.to_string()),
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
            let response_limit = page.limit.unwrap_or(super::PAGE_SIZE).max(1);
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
        for _ in 0..super::MAX_PAGES {
            let params = [
                ("limit", super::PAGE_SIZE.to_string()),
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
            let response_limit = page.limit.unwrap_or(super::PAGE_SIZE).max(1);
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
