use crate::policy::Capability;

pub(super) fn supports(capability: Capability) -> bool {
    matches!(
        capability,
        Capability::IssueRead
            | Capability::IssueSearch
            | Capability::IssueCreate
            | Capability::IssueUpdateBody
            | Capability::IssueClose
            | Capability::CommentCreate
            | Capability::CommentRead
            | Capability::CommentFindMarker
            | Capability::ProjectRead
            | Capability::IssueStatusRead
            | Capability::VersionRead
    )
}
