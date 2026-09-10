use crate::policy::Capability;
use crate::providers::config::RedmineProvider;

impl RedmineProvider {
    pub(crate) fn capabilities(&self) -> crate::providers::ProviderCapabilities {
        crate::providers::ProviderCapabilities {
            issue_lifecycle: true,
            comments: true,
            repository_creation: false,
        }
    }

    pub(crate) fn supports(&self, capability: Capability) -> bool {
        // Phase 4 parity (issue 257): the uniform
        // `IssueAttachmentUpload = false` row now lives on the
        // inherent provider. Phase 1 only intercepted the row at the
        // dispatcher surface so the CLI/MCP guard stayed single-
        // sourced; Phase 4 sinks the value here so the dispatcher arm
        // can forward to the provider directly. The underlying
        // `upload_attachment` method stays compiled for callers that
        // reach the inherent surface (e.g. the legacy
        // `contract_tests/attachments.rs` wire-shape tests), but no
        // command path reaches it because every CLI/MCP entry point
        // is gated by `provider.supports(...)` and now rejects the
        // capability uniformly.
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
            Capability::ProjectRead | Capability::ProjectCreate | Capability::IssueStatusRead => {
                true
            }
            Capability::VersionRead => true,
            Capability::RelationRead | Capability::RelationCreate | Capability::RelationDelete => {
                true
            }
            Capability::RepoCreate => false,
        }
    }
}
