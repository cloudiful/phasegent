#[allow(unused_imports)]
use crate::ci_model::{
    CiInspectOutput, CiInspectRequest, CiJobLogsOutput, CiJobsOutput, CiRunSummary, CiRunsFilter,
    CiRunsOutput,
};
#[allow(unused_imports)]
use crate::command::{CiCommand, RepoCommand};
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
    CiProvider, GitlabProvider, IssueProvider, ProviderCapabilities, ProviderKind,
    RedmineIssueStatus, RedmineMetadataProvider, RedmineProject, RedmineProvider, RedmineVersion,
    RepoProvider,
};

impl RepoProvider for GitlabProvider {
    type Error = ForgejoError;

    fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, Self::Error> {
        // Phase 3 wires the GitLab adapter straight through to the
        // inherent `GitlabProvider::create_repo` method so a direct
        // trait call cannot recurse into the adapter. The CLI guard
        // still routes this provider only when the operator asked
        // for GitLab and supplied `--private`; private enforcement
        // and the namespace resolver live inside the provider.
        GitlabProvider::create_repo(self, target, private, description, auto_init)
    }
}

impl RepoProvider for ForgejoProvider {
    type Error = ForgejoError;

    fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, Self::Error> {
        ForgejoProvider::create_repo(self, target, private, description, auto_init)
    }
}

impl RepoProvider for RedmineProvider {
    type Error = ForgejoError;

    fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, Self::Error> {
        RedmineProvider::create_repo(self, target, private, description, auto_init)
    }
}

impl RepoProvider for ProviderDispatcher {
    type Error = ForgejoError;

    fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, Self::Error> {
        match self {
            Self::Forgejo(provider) => {
                provider.create_repo(target, private, description, auto_init)
            }
            Self::Redmine(provider) => {
                provider.create_repo(target, private, description, auto_init)
            }
            Self::Gitlab(provider) => provider.create_repo(target, private, description, auto_init),
        }
    }
}
