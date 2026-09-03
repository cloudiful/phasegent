#[allow(unused_imports)]
use crate::policy::Capability;
#[allow(unused_imports)]
use crate::providers::ProviderDispatcher;
#[allow(unused_imports)]
use crate::providers::api::{CommentOutput, ForgejoError, IssueSummary, RepoSummary};
#[allow(unused_imports)]
use crate::providers::forgejo::ForgejoConfig;
#[allow(unused_imports)]
use crate::providers::forgejo::ForgejoProvider;
#[allow(unused_imports)]
use crate::providers::{
    GitlabProvider, IssueProvider, ProviderCapabilities, ProviderKind, RedmineIssueStatus,
    RedmineMetadataProvider, RedmineProject, RedmineProvider, RedmineVersion, RepoProvider,
};

impl RedmineMetadataProvider for ForgejoProvider {
    type Error = ForgejoError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        Err(ForgejoError::not_supported("forgejo", "project list"))
    }

    fn create_project(
        &self,
        _name: &str,
        _identifier: &str,
        _description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        Err(ForgejoError::not_supported("forgejo", "project create"))
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        Err(ForgejoError::not_supported("forgejo", "issue status list"))
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        Err(ForgejoError::not_supported("forgejo", "version list"))
    }
}

impl RedmineMetadataProvider for RedmineProvider {
    type Error = ForgejoError;

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

// ============================================================================
// Phase-2 GitLab issue / note / label foundation. The dispatch wiring
// forwards every IssueProvider / RedmineMetadataProvider / RepoProvider
// trait call straight to the real
// [`crate::providers::gitlab::GitlabProvider`] implementation. Capability flags
// for not-supported operations stay false so the shared CLI surfaces a
// structured not-supported error before any HTTP traffic.
// ============================================================================

impl RedmineMetadataProvider for GitlabProvider {
    type Error = ForgejoError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        Err(ForgejoError::not_supported("gitlab", "project list"))
    }

    fn create_project(
        &self,
        _name: &str,
        _identifier: &str,
        _description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        Err(ForgejoError::not_supported("gitlab", "project create"))
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        Err(ForgejoError::not_supported("gitlab", "issue status list"))
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        Err(ForgejoError::not_supported("gitlab", "version list"))
    }
}

impl RedmineMetadataProvider for ProviderDispatcher {
    type Error = ForgejoError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.list_projects(),
            Self::Redmine(provider) => provider.list_projects(),
            Self::Gitlab(provider) => provider.list_projects(),
        }
    }

    fn create_project(
        &self,
        name: &str,
        identifier: &str,
        description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.create_project(name, identifier, description),
            Self::Redmine(provider) => provider.create_project(name, identifier, description),
            Self::Gitlab(provider) => provider.create_project(name, identifier, description),
        }
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.list_issue_statuses(),
            Self::Redmine(provider) => provider.list_issue_statuses(),
            Self::Gitlab(provider) => provider.list_issue_statuses(),
        }
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.list_project_versions(),
            Self::Redmine(provider) => provider.list_project_versions(),
            Self::Gitlab(provider) => provider.list_project_versions(),
        }
    }
}
