#[allow(unused_imports)]
use crate::command::RepoCommand;
#[allow(unused_imports)]
use crate::policy::Capability;
#[allow(unused_imports)]
use crate::providers::api::{CommentOutput, IssueSummary, ProviderError, RepoSummary};
use crate::providers::hierarchy::HierarchyPage;
use crate::providers::hierarchy::WorkItemRef;
use crate::providers::local::LocalProvider;
#[allow(unused_imports)]
use crate::providers::{
    GitlabProvider, HierarchyNode, IssueProvider, ProviderCapabilities, ProviderKind,
    RedmineIssueStatus, RedmineMetadataProvider, RedmineProject, RedmineProvider, RedmineVersion,
    RepoProvider,
};

pub enum ProviderDispatcher {
    Redmine(RedmineProvider),
    Gitlab(GitlabProvider),
    /// Local backend (SQLite-first, PG reserved).
    Local(LocalProvider),
}
impl ProviderDispatcher {
    pub fn redmine(
        role: crate::policy::Role,
        config: crate::providers::config::RedmineConfig,
    ) -> Result<Self, ProviderError> {
        Ok(Self::Redmine(RedmineProvider::for_role(role, config)?))
    }

    pub fn gitlab(
        role: crate::policy::Role,
        config: crate::providers::config::GitlabConfig,
    ) -> Result<Self, ProviderError> {
        Ok(Self::Gitlab(GitlabProvider::for_role(role, config)?))
    }

    /// Local backend. SQLite opens synchronously with no credentials;
    /// the PostgreSQL variant stays reserved in `PgLocalProvider`.
    pub fn local(provider: LocalProvider) -> Self {
        Self::Local(provider)
    }

    /// Drive a `RepoCommand::Create` through whichever provider arm
    /// resolved. Redmine still surfaces a structured not-supported
    /// error. The provider-side enforcement of `--private` and
    /// namespace resolution stays inside each provider so this
    /// dispatcher stays thin.
    pub fn create_repo_for_command(
        &self,
        command: &RepoCommand,
        _role: crate::policy::Role,
        _api_base: Option<&str>,
    ) -> Result<RepoSummary, ProviderError> {
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
            Self::Redmine(_) => ProviderKind::Redmine,
            Self::Gitlab(_) => ProviderKind::Gitlab,
            Self::Local(_) => ProviderKind::Local,
        }
    }

    /// Read-only native hierarchy view. Redmine serves parent/children from
    /// native fields; GitLab serves Work Item hierarchy via GraphQL. Other
    /// providers return structured `not_supported`. Never falls back to
    /// relations.
    #[allow(dead_code)]
    pub fn get_hierarchy(&self, number: u64) -> Result<HierarchyNode, ProviderError> {
        match self {
            Self::Redmine(redmine) => redmine.get_hierarchy(number),
            Self::Gitlab(gitlab) => gitlab.get_hierarchy(number),
            other => Err(ProviderError::not_supported(
                other.kind().as_str(),
                "issue hierarchy get",
            )),
        }
    }

    /// Bounded read-only native hierarchy view plus an explicit truncation
    /// indicator. Redmine reports the complete child list (`false`); GitLab
    /// reports the child connection `pageInfo.hasNextPage`. Other providers
    /// return structured `not_supported`. Never falls back to relations.
    pub fn get_hierarchy_page(&self, number: u64) -> Result<HierarchyPage, ProviderError> {
        match self {
            Self::Redmine(redmine) => redmine.get_hierarchy_page(number),
            Self::Gitlab(gitlab) => gitlab.get_hierarchy_page(number),
            other => Err(ProviderError::not_supported(
                other.kind().as_str(),
                "issue hierarchy get",
            )),
        }
    }

    /// Typed parent write from bare numeric ids. Redmine assigns the native
    /// `parent_issue_id`; GitLab resolves both items' native kind/scope
    /// through hierarchy reads before issuing the widget mutation. Other
    /// providers return structured `not_supported`.
    pub fn set_hierarchy_parent_by_id(&self, child: u64, parent: u64) -> Result<(), ProviderError> {
        match self {
            Self::Redmine(redmine) => redmine.set_hierarchy_parent(child, parent),
            Self::Gitlab(gitlab) => gitlab.set_hierarchy_parent_by_id(child, parent),
            other => Err(ProviderError::not_supported(
                other.kind().as_str(),
                "issue hierarchy update",
            )),
        }
    }

    /// Typed parent write from already-resolved refs (Epic-to-Issue,
    /// Issue-to-Task). The CLI resolves bare ids through
    /// [`Self::set_hierarchy_parent_by_id`]; this entry stays for callers
    /// that already hold native refs. Other providers return structured
    /// `not_supported`.
    #[allow(dead_code)]
    pub fn set_hierarchy_parent(
        &self,
        child: WorkItemRef,
        parent: WorkItemRef,
    ) -> Result<(), ProviderError> {
        match self {
            Self::Redmine(redmine) => redmine.set_hierarchy_parent(child.id, parent.id),
            Self::Gitlab(gitlab) => gitlab.set_hierarchy_parent(&child, &parent),
            other => Err(ProviderError::not_supported(
                other.kind().as_str(),
                "issue hierarchy update",
            )),
        }
    }

    /// Clear the native parent from bare numeric ids. Redmine writes an
    /// explicit null `parent_issue_id`; GitLab issues the widget mutation
    /// with a null parent. Other providers return structured
    /// `not_supported`. Never touches relations.
    pub fn unset_hierarchy_parent_by_id(&self, child: u64) -> Result<(), ProviderError> {
        match self {
            Self::Redmine(redmine) => redmine.unset_hierarchy_parent(child),
            Self::Gitlab(gitlab) => gitlab.unset_hierarchy_parent_by_id(child),
            other => Err(ProviderError::not_supported(
                other.kind().as_str(),
                "issue hierarchy update",
            )),
        }
    }
}
