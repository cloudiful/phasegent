#[allow(unused_imports)]
use crate::policy::Capability;
#[allow(unused_imports)]
use crate::providers::api::{CommentOutput, IssueSummary, PhasegentError};
use crate::providers::local::LocalProvider;
#[allow(unused_imports)]
use crate::providers::{
    IssueProvider, ProviderCapabilities, ProviderKind, RedmineIssueStatus, RedmineMetadataProvider,
    RedmineProject, RedmineProvider, RedmineVersion,
};

/// The resolved tracking provider for one invocation. Only the two
/// supported providers exist; a retired name never reaches this type
/// because [`crate::providers::config::resolve_kind`] rejects it first.
pub enum ProviderDispatcher {
    Redmine(RedmineProvider),
    /// Local backend (SQLite-first, PG reserved).
    Local(LocalProvider),
}

impl ProviderDispatcher {
    pub fn redmine(
        role: crate::policy::Role,
        config: crate::providers::config::RedmineConfig,
    ) -> Result<Self, PhasegentError> {
        Ok(Self::Redmine(RedmineProvider::for_role(role, config)?))
    }

    /// Local backend. SQLite opens synchronously with no credentials;
    /// the PostgreSQL variant stays reserved in `PgLocalProvider`.
    pub fn local(provider: LocalProvider) -> Self {
        Self::Local(provider)
    }

    pub const fn kind(&self) -> ProviderKind {
        match self {
            Self::Redmine(_) => ProviderKind::Redmine,
            Self::Local(_) => ProviderKind::Local,
        }
    }
}
