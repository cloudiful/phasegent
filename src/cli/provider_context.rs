//! The complete typed provider invocation context (issue 1201 P3).
//!
//! Every provider-dependent production route carries the raw optional
//! `--provider` plus the endpoint/repository/project/close-status overrides as
//! one value. Resolution always keeps the raw optional provider so an omitted
//! selection can never be promoted to an explicit one, and every provider
//! (including a configured Forgejo default) resolves through the same
//! selection chain.

use crate::command::Invocation;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::providers::{ProviderCapabilities, ProviderDispatcher, ProviderKind};

#[derive(Clone, Debug, Default)]
pub(crate) struct ProviderContext {
    /// The raw `--provider` flag; `None` means the invocation did not select
    /// one and must stay default-resolved.
    pub provider: Option<ProviderKind>,
    pub api_base: Option<String>,
    pub repository: Option<String>,
    pub project_id: Option<String>,
    pub close_status_id: Option<String>,
}

impl ProviderContext {
    pub(crate) fn from_invocation(invocation: &Invocation) -> Self {
        Self {
            provider: invocation.provider,
            api_base: invocation.api_base.clone(),
            repository: invocation.repository.clone(),
            project_id: invocation.project_id.clone(),
            close_status_id: invocation.close_status_id.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        provider: Option<ProviderKind>,
        api_base: Option<&str>,
        repository: Option<&str>,
        project_id: Option<&str>,
        close_status_id: Option<&str>,
    ) -> Self {
        Self {
            provider,
            api_base: api_base.map(str::to_owned),
            repository: repository.map(str::to_owned),
            project_id: project_id.map(str::to_owned),
            close_status_id: close_status_id.map(str::to_owned),
        }
    }

    /// Validate the raw selection without credentials or HTTP: an explicit
    /// Forgejo rejects project/close-status overrides. Every provider resolves
    /// through the common selection chain, so a configured default is admitted
    /// exactly like an explicit flag.
    pub(crate) fn validate(&self, role: Role) -> Result<(), PhasegentError> {
        let kind = crate::providers::config::resolve_kind(role, self.provider)?;
        if kind == ProviderKind::Forgejo
            && (self.project_id.is_some() || self.close_status_id.is_some())
        {
            return Err(PhasegentError::config(
                "Forgejo does not support --project-id or --close-status-id",
            ));
        }
        if kind == ProviderKind::Gitlab
            && (self.project_id.is_some() || self.close_status_id.is_some())
        {
            return Err(PhasegentError::config(
                "GitLab does not support --project-id or --close-status-id",
            ));
        }
        Ok(())
    }

    /// Resolve the dispatcher with the raw selection intact through the common
    /// selection chain. Incompatible Forgejo flags fail here, before any HTTP
    /// or mutation.
    pub(crate) fn resolve(&self, role: Role) -> Result<ProviderDispatcher, PhasegentError> {
        crate::cli::provider_for(
            role,
            self.provider,
            self.api_base.as_deref(),
            self.repository.as_deref(),
            self.project_id.as_deref(),
            self.close_status_id.as_deref(),
        )
    }

    /// The provider scope the raw selection resolves to, without building a
    /// credentialed provider and without any HTTP. `None` means the selection
    /// carries no provable scope, so callers keep normal default routing.
    pub(crate) fn resolved_scope(
        &self,
        role: Role,
    ) -> Result<Option<ResolvedScope>, PhasegentError> {
        self.validate(role)?;
        let kind = crate::providers::config::resolve_kind(role, self.provider)?;
        match kind {
            ProviderKind::Forgejo => {
                let config = crate::providers::forgejo::ForgejoConfig::resolve(
                    role,
                    self.api_base.as_deref(),
                    self.repository.as_deref(),
                )?;
                Ok(Some(ResolvedScope {
                    provider: kind,
                    project: config.repository().to_owned(),
                }))
            }
            ProviderKind::Gitlab => {
                let config = crate::providers::gitlab::GitlabConfig::resolve(
                    role,
                    self.api_base.as_deref(),
                    self.repository.as_deref(),
                )?;
                Ok(Some(ResolvedScope {
                    provider: kind,
                    project: config.repository().to_owned(),
                }))
            }
            ProviderKind::Redmine => Ok(crate::providers::RedmineConfig::resolve(
                role,
                self.api_base.as_deref(),
                self.project_id.as_deref(),
                self.close_status_id.as_deref(),
            )
            .ok()
            .and_then(|config| config.project_id)
            .map(|project| ResolvedScope {
                provider: kind,
                project,
            })),
            ProviderKind::Local => Ok(Some(ResolvedScope {
                provider: kind,
                project: "default".to_owned(),
            })),
        }
    }

    /// The fixed capability facts of the resolved provider, available without
    /// credentials or HTTP so lifecycle decisions stay local.
    pub(crate) fn capabilities(&self, role: Role) -> Result<ProviderCapabilities, PhasegentError> {
        let kind = crate::providers::config::resolve_kind(role, self.provider)?;
        Ok(capabilities_for(kind))
    }
}

/// The fixed capability facts of one provider kind, available without
/// credentials or HTTP.
pub(crate) fn capabilities_for(kind: ProviderKind) -> ProviderCapabilities {
    match kind {
        ProviderKind::Redmine => crate::providers::RedmineProvider::CAPABILITIES,
        ProviderKind::Local => crate::providers::local::LocalProvider::CAPABILITIES,
        ProviderKind::Forgejo => crate::providers::forgejo::ForgejoProvider::CAPABILITIES,
        ProviderKind::Gitlab => crate::providers::gitlab::GitlabProvider::CAPABILITIES,
    }
}

/// The provider and project scope a raw selection resolves to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedScope {
    pub provider: ProviderKind,
    pub project: String,
}
