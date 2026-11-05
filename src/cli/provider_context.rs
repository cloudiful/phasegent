//! The complete typed provider invocation context (issue 1201 P3).
//!
//! Every provider-dependent production route carries the raw optional
//! `--provider` plus the endpoint/repository/project/close-status overrides as
//! one value. Resolution always keeps the raw optional provider so an omitted
//! selection can never be promoted to an explicit one, and explicit Local or
//! Redmine always wins over environment or stored Forgejo defaults.

use crate::command::Invocation;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::providers::{ProviderDispatcher, ProviderKind};

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

    /// Validate the raw selection without credentials or HTTP: a resolved
    /// Forgejo default is prohibited unless `--provider forgejo` was explicit,
    /// and explicit Forgejo rejects project/close-status overrides.
    pub(crate) fn validate(&self, role: Role) -> Result<(), PhasegentError> {
        let kind = crate::providers::config::resolve_kind(role, self.provider)?;
        if kind == ProviderKind::Forgejo {
            if self.provider != Some(ProviderKind::Forgejo) {
                return Err(PhasegentError::config(
                    "Forgejo requires explicit --provider forgejo",
                ));
            }
            if self.project_id.is_some() || self.close_status_id.is_some() {
                return Err(PhasegentError::config(
                    "Forgejo does not support --project-id or --close-status-id",
                ));
            }
        }
        Ok(())
    }

    /// Resolve the dispatcher with the raw selection intact. Omitted Forgejo
    /// defaults and incompatible explicit flags fail here, before any HTTP or
    /// mutation.
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

    /// The validated provider-owned Forgejo repository for an explicit
    /// selection, so a missing `--repository` still resolves through the
    /// dedicated Forgejo config for preview and recovery.
    pub(crate) fn owned_forgejo_repository(
        &self,
        role: Role,
    ) -> Result<Option<String>, PhasegentError> {
        self.validate(role)?;
        if self.provider != Some(ProviderKind::Forgejo) {
            return Ok(None);
        }
        let config = crate::providers::forgejo::ForgejoConfig::resolve(
            role,
            self.api_base.as_deref(),
            self.repository.as_deref(),
        )?;
        Ok(Some(config.repository().to_owned()))
    }
}
