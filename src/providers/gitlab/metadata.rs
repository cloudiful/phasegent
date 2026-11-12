use super::{GitlabProvider, model::Milestone};
use crate::pagination::{self, PageRequest, Paged};
use crate::providers::api::PhasegentError;
use crate::providers::{
    RedmineIssueStatus, RedmineMetadataProvider, RedmineProject, RedmineVersion,
};

impl RedmineMetadataProvider for GitlabProvider {
    type Error = PhasegentError;
    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        let project = self.project()?;
        Ok(vec![RedmineProject {
            id: project.id,
            name: project.name,
            identifier: project.path_with_namespace,
            description: project.description.unwrap_or_default(),
            status: None,
            is_public: project
                .visibility
                .as_deref()
                .map(|visibility| visibility.eq_ignore_ascii_case("public")),
            inherit_members: None,
            created_on: None,
            updated_on: None,
        }])
    }
    fn list_projects_page(&self, page: PageRequest) -> Result<Paged<RedmineProject>, Self::Error> {
        Ok(pagination::slice(self.list_projects()?, page))
    }
    fn list_versions_page(&self, page: PageRequest) -> Result<Paged<RedmineVersion>, Self::Error> {
        Ok(pagination::slice(self.list_project_versions()?, page))
    }
    fn create_project(
        &self,
        _name: &str,
        _identifier: &str,
        _description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        Err(PhasegentError::not_supported("gitlab", "project create"))
    }
    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        Ok(super::status::native_states())
    }
    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        self.project()?;
        let milestones: Vec<Milestone> = self.collect(
            &["projects", self.config.repository(), "milestones"],
            &[("state", "all".to_owned())],
            "version list",
        )?;
        Ok(milestones
            .into_iter()
            .map(|milestone| RedmineVersion {
                id: milestone.id,
                name: milestone.title,
                status: milestone.state,
                due_date: milestone.due_date,
            })
            .collect())
    }
}
