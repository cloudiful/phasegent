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
use crate::providers::local::LocalProvider;
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
        // Phase 2 parity (issue 257): GitLab's `GET /projects` is the
        // equivalent read for `ProjectRead`. The mapping from the
        // GitLab `ApiProject` payload onto the shared `RedmineProject`
        // shape lives in
        // `crate::providers::gitlab::impl::repo`.
        GitlabProvider::list_projects(self)
    }

    fn create_project(
        &self,
        _name: &str,
        _identifier: &str,
        _description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        // Phase 2 parity (issue 257): GitLab's project-create equivalent
        // lives on the `repo create` path (POST `/projects` via
        // `RepoProvider::create_repo`). The `project create` CLI command
        // stays not-supported for GitLab so there is one and only one
        // entry point for the underlying endpoint.
        Err(ForgejoError::not_supported("gitlab", "project create"))
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        // Phase 2 parity (issue 257): GitLab has no native status enum;
        // the workflow is encoded as project labels. The orchestrator
        // surfaces the canonical catalogue
        // (`workflow::*` → status entries) so the shared `status list`
        // CLI command and any downstream planning flow see the same
        // eight statuses the Redmine catalogue does.
        GitlabProvider::list_workflow_statuses(self)
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        // Phase 2 parity (issue 257): GitLab milestones map onto the
        // shared `RedmineVersion` shape; `GET /projects/:id/milestones`
        // is the equivalent read for `VersionRead`.
        GitlabProvider::list_milestones(self)
    }
}

impl RedmineMetadataProvider for ProviderDispatcher {
    type Error = ForgejoError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.list_projects(),
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
            Self::Forgejo(provider) => provider.create_project(name, identifier, description),
            Self::Redmine(provider) => provider.create_project(name, identifier, description),
            Self::Gitlab(provider) => provider.create_project(name, identifier, description),
            Self::Local(provider) => provider.create_project(name, identifier, description),
        }
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.list_issue_statuses(),
            Self::Redmine(provider) => provider.list_issue_statuses(),
            Self::Gitlab(provider) => provider.list_issue_statuses(),
            Self::Local(provider) => provider.list_issue_statuses(),
        }
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.list_project_versions(),
            Self::Redmine(provider) => provider.list_project_versions(),
            Self::Gitlab(provider) => provider.list_project_versions(),
            Self::Local(provider) => provider.list_project_versions(),
        }
    }
}

impl RedmineMetadataProvider for LocalProvider {
    type Error = ForgejoError;

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
