//! GitLab provider core: struct, factory, path builders, capabilities.

use crate::infra::storage::Storage;
use crate::policy::Capability;
use crate::providers::api::ForgejoError;
use crate::providers::config::GitlabConfig;
use crate::providers::gitlab::http::GitlabHttp;

/// Concrete GitLab provider. The struct is held by the
/// `ProviderDispatcher::Gitlab` arm; the surrounding CLI talks to it
/// through the shared `IssueProvider` trait and a handful of GitLab-
/// specific helpers (`set_workflow_status`, `tracker_label`, etc.).
///
/// Public so `provider_config.rs` can re-export it under the same name
/// for the `crate::providers::ProviderDispatcher` enum. The struct is
/// an opaque transport; callers should drive it via the trait, not
/// reach into its fields, so widening visibility here does not
/// expose anything new beyond what the dispatcher already surfaces.
#[allow(dead_code)]
pub struct GitlabProvider {
    pub(crate) config: GitlabConfig,
    pub(crate) http: GitlabHttp,
}

impl std::fmt::Debug for GitlabProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The HTTP struct already redacts the token in its `Debug`
        // impl; delegate to it so the provider-level Debug cannot
        // accidentally expose it either.
        formatter
            .debug_struct("GitlabProvider")
            .field("config", &self.config)
            .field("http", &self.http)
            .finish()
    }
}

impl GitlabProvider {
    pub(crate) fn for_role(
        role: crate::policy::Role,
        config: GitlabConfig,
    ) -> Result<Self, ForgejoError> {
        let storage = Storage::open().map_err(ForgejoError::config)?;
        let token = crate::auth::gitlab_token(role, &storage).map_err(ForgejoError::auth)?;
        Self::new(config, token)
    }

    pub(crate) fn new(config: GitlabConfig, token: String) -> Result<Self, ForgejoError> {
        let http = GitlabHttp::new(config.api_base.clone(), token)?;
        Ok(Self { config, http })
    }

    /// Numeric project id used in every per-project URL.
    pub(crate) const fn project_id(&self) -> u64 {
        self.config.project_id
    }

    // -- HTTP path builders --------------------------------------------------

    pub(crate) fn issues_path(&self) -> String {
        format!("projects/{}/issues", self.project_id())
    }

    pub(crate) fn issue_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}", self.project_id())
    }

    pub(crate) fn notes_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}/notes", self.project_id())
    }

    pub(crate) fn note_path(&self, iid: u64, note_id: u64) -> String {
        format!(
            "projects/{}/issues/{iid}/notes/{note_id}",
            self.project_id()
        )
    }

    pub(crate) fn labels_path(&self) -> String {
        format!("projects/{}/labels", self.project_id())
    }

    pub(crate) fn spent_time_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}/add_spent_time", self.project_id())
    }

    pub(crate) fn time_estimate_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}/time_estimate", self.project_id())
    }

    pub(crate) fn issue_links_path(&self, iid: u64) -> String {
        format!("projects/{}/issues/{iid}/links", self.project_id())
    }

    pub(crate) fn issue_link_path(&self, source_issue_iid: u64, link_id: u64) -> String {
        format!(
            "projects/{}/issues/{source_issue_iid}/links/{link_id}",
            self.project_id()
        )
    }

    pub(crate) fn projects_path(&self) -> String {
        "projects".to_owned()
    }

    // -- Not-supported helpers for later phases ------------------------------

    #[allow(dead_code)]
    pub(crate) fn unsupported<T>(&self, operation: &str) -> Result<T, ForgejoError> {
        Err(ForgejoError::not_supported("gitlab", operation))
    }
}

// -- Capability surface ----------------------------------------------------

impl GitlabProvider {
    /// Capability table for GitLab. Phase 3 lights up repository
    /// creation alongside the Phase 2 issue / comment / workflow
    /// surface. Phase 4 lifts the relation surface from not-supported
    /// to native so the shared CLI can dispatch `relation
    /// list/create/delete` to the GitLab provider.
    pub(crate) fn capabilities(&self) -> crate::providers::ProviderCapabilities {
        crate::providers::ProviderCapabilities {
            issue_lifecycle: true,
            comments: true,
            repository_creation: true,
        }
    }

    pub(crate) fn supports(&self, capability: Capability) -> bool {
        match capability {
            Capability::IssueRead
            | Capability::IssueSearch
            | Capability::IssueCreate
            | Capability::IssueUpdateBody
            | Capability::IssueClose => true,
            Capability::IssueAttachmentUpload => false,
            Capability::CommentCreate | Capability::CommentRead | Capability::CommentFindMarker => {
                true
            }
            Capability::RepoCreate => true,
            Capability::RelationRead | Capability::RelationCreate | Capability::RelationDelete => {
                true
            }
            Capability::ProjectRead
            | Capability::ProjectCreate
            | Capability::IssueStatusRead
            | Capability::VersionRead => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Capability;
    use crate::providers::config::GitlabConfig;

    #[test]
    fn capabilities_match_phase_4_scope() {
        let provider = GitlabProvider::new(
            GitlabConfig::new("https://gitlab.example/api/v4", 42),
            "test-token".to_owned(),
        )
        .unwrap();
        let caps = provider.capabilities();
        assert!(caps.issue_lifecycle);
        assert!(caps.comments);
        assert!(caps.repository_creation);
        assert!(provider.supports(Capability::IssueRead));
        assert!(provider.supports(Capability::CommentCreate));
        assert!(provider.supports(Capability::RepoCreate));
        // Phase 4: relations are native on GitLab.
        assert!(provider.supports(Capability::RelationRead));
        assert!(provider.supports(Capability::RelationCreate));
        assert!(provider.supports(Capability::RelationDelete));
        assert!(!provider.supports(Capability::IssueStatusRead));
        assert!(!provider.supports(Capability::ProjectRead));
    }
}
