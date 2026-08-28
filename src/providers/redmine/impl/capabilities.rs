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
        match capability {
            Capability::IssueRead
            | Capability::IssueSearch
            | Capability::IssueCreate
            | Capability::IssueUpdateBody
            | Capability::IssueClose => true,
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
            Capability::RepoCreate | Capability::CiRead => false,
        }
    }
}
