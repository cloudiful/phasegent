use super::{ForgejoProvider, model::Milestone};
use crate::pagination::{self, PageRequest, Paged};
use crate::providers::api::PhasegentError;
use crate::providers::{
    RedmineIssueStatus, RedmineMetadataProvider, RedmineProject, RedmineVersion,
};

impl RedmineMetadataProvider for ForgejoProvider {
    type Error = PhasegentError;
    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        let repo = self.repository()?;
        Ok(vec![RedmineProject {
            id: repo.id,
            name: repo.name,
            identifier: repo.full_name,
            description: repo.description.unwrap_or_default(),
            status: None,
            is_public: Some(!repo.private),
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
        Err(PhasegentError::not_supported("forgejo", "project create"))
    }
    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        // The native catalogue is synthetic: the two states need no server
        // data, so the read performs no request at all.
        Ok(super::status::native_states())
    }
    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        self.repository()?;
        let milestones: Vec<Milestone> = self.collect(
            &["milestones"],
            &[("state", "all".to_owned())],
            "version list",
        )?;
        Ok(milestones
            .into_iter()
            .map(|m| RedmineVersion {
                id: m.id,
                name: m.title,
                status: m.state,
                due_date: m.due_on,
            })
            .collect())
    }
}
