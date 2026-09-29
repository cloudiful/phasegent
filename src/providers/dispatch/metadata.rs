#[allow(unused_imports)]
use crate::policy::Capability;
#[allow(unused_imports)]
use crate::providers::ProviderDispatcher;
#[allow(unused_imports)]
use crate::providers::api::{CommentOutput, IssueSummary, ProviderError, RepoSummary};
use crate::providers::local::LocalProvider;
#[allow(unused_imports)]
use crate::providers::{
    GitlabProvider, IssueProvider, ProviderCapabilities, ProviderKind, RedmineIssueStatus,
    RedmineMetadataProvider, RedmineProject, RedmineProvider, RedmineVersion, RepoProvider,
};

impl RedmineMetadataProvider for RedmineProvider {
    type Error = ProviderError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        RedmineProvider::list_projects(self)
    }

    fn create_project(
        &self,
        name: &str,
        identifier: &str,
        description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        RedmineProvider::create_project(self, name, identifier, description)
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        RedmineProvider::list_issue_statuses(self)
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        RedmineProvider::list_versions(self)
    }
}

impl RedmineMetadataProvider for GitlabProvider {
    type Error = ProviderError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        GitlabProvider::list_projects(self)
    }

    fn create_project(
        &self,
        _name: &str,
        _identifier: &str,
        _description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        Err(ProviderError::not_supported("gitlab", "project create"))
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        GitlabProvider::list_workflow_statuses(self)
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        GitlabProvider::list_milestones(self)
    }
}

impl RedmineMetadataProvider for ProviderDispatcher {
    type Error = ProviderError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.list_projects(),
            Self::Gitlab(provider) => provider.list_projects(),
            Self::Local(provider) => provider.list_projects(),
        }
    }

    fn create_project(
        &self,
        name: &str,
        identifier: &str,
        description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.create_project(name, identifier, description),
            Self::Gitlab(provider) => provider.create_project(name, identifier, description),
            Self::Local(provider) => provider.create_project(name, identifier, description),
        }
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.list_issue_statuses(),
            Self::Gitlab(provider) => provider.list_issue_statuses(),
            Self::Local(provider) => provider.list_issue_statuses(),
        }
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.list_project_versions(),
            Self::Gitlab(provider) => provider.list_project_versions(),
            Self::Local(provider) => provider.list_project_versions(),
        }
    }
}

impl RedmineMetadataProvider for LocalProvider {
    type Error = ProviderError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        LocalProvider::list_projects(self)
    }

    fn create_project(
        &self,
        name: &str,
        identifier: &str,
        description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        LocalProvider::create_project(self, name, identifier, description)
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        LocalProvider::list_issue_statuses(self)
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        LocalProvider::list_versions(self)
    }
}
