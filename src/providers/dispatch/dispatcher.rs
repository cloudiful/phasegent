#[allow(unused_imports)]
use crate::command::RepoCommand;
#[allow(unused_imports)]
use crate::policy::Capability;
#[allow(unused_imports)]
use crate::providers::api::{CommentOutput, ForgejoError, IssueSummary, RepoSummary};
use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
#[allow(unused_imports)]
use crate::providers::{
    GitlabProvider, IssueProvider, ProviderCapabilities, ProviderKind, RedmineIssueStatus,
    RedmineMetadataProvider, RedmineProject, RedmineProvider, RedmineVersion, RepoProvider,
};

pub enum ProviderDispatcher {
    Forgejo(ForgejoProvider),
    Redmine(RedmineProvider),
    /// Phase-1 GitLab foundation. The dispatcher already routes every
    /// trait call to GitlabProvider's not-supported stubs so subsequent
    /// phases only need to replace the implementation, not the dispatch
    /// wiring.
    Gitlab(GitlabProvider),
}

impl ProviderDispatcher {
    pub fn for_role(
        role: crate::policy::Role,
        config: ForgejoConfig,
    ) -> Result<Self, ForgejoError> {
        Ok(Self::Forgejo(ForgejoProvider::for_role(role, config)?))
    }

    pub fn redmine(
        role: crate::policy::Role,
        config: crate::providers::config::RedmineConfig,
    ) -> Result<Self, ForgejoError> {
        Ok(Self::Redmine(RedmineProvider::for_role(role, config)?))
    }

    pub fn gitlab(
        role: crate::policy::Role,
        config: crate::providers::config::GitlabConfig,
    ) -> Result<Self, ForgejoError> {
        Ok(Self::Gitlab(GitlabProvider::for_role(role, config)?))
    }

    /// Drive a `RepoCommand::Create` through whichever provider arm
    /// resolved. Phase 3 adds GitLab support; Redmine still surfaces a
    /// structured not-supported error. The provider-side enforcement
    /// of `--private` and namespace resolution stays inside each
    /// provider so this dispatcher stays thin.
    pub fn create_repo_for_command(
        &self,
        command: &RepoCommand,
        _role: crate::policy::Role,
        _api_base: Option<&str>,
        _repository: Option<&str>,
    ) -> Result<RepoSummary, ForgejoError> {
        let RepoCommand::Create {
            target,
            private,
            description,
            auto_init,
        } = command;
        RepoProvider::create_repo(self, target, *private, description, *auto_init)
    }

    pub const fn kind(&self) -> ProviderKind {
        match self {
            Self::Forgejo(_) => ProviderKind::Forgejo,
            Self::Redmine(_) => ProviderKind::Redmine,
            Self::Gitlab(_) => ProviderKind::Gitlab,
        }
    }
}
