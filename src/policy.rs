//! Role and capability policy. The five roles (`admin`, `orchestrator`,
//! `executor`, `reviewer`, `tester`) gate every CLI/MCP primitive and
//! the `Capability` enum is the closed set of operations the CLI
//! exposes to roles and providers.

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Admin,
    Orchestrator,
    Executor,
    Reviewer,
    Tester,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Orchestrator => "orchestrator",
            Self::Executor => "executor",
            Self::Reviewer => "reviewer",
            Self::Tester => "tester",
        }
    }

    pub const fn allows(self, capability: Capability) -> bool {
        match self {
            Self::Orchestrator => true,
            Self::Admin => matches!(
                capability,
                Capability::ProjectRead
                    | Capability::ProjectCreate
                    | Capability::IssueStatusRead
                    | Capability::VersionRead
            ),
            Self::Executor => matches!(
                capability,
                Capability::IssueRead
                    | Capability::CommentRead
                    | Capability::CommentFindMarker
                    | Capability::CommentCreate
                    | Capability::ProjectRead
                    | Capability::IssueStatusRead
                    | Capability::VersionRead
                    | Capability::RelationRead
            ),
            Self::Reviewer => matches!(
                capability,
                Capability::IssueRead
                    | Capability::CommentRead
                    | Capability::CommentFindMarker
                    | Capability::CommentCreate
                    | Capability::ProjectRead
                    | Capability::IssueStatusRead
                    | Capability::VersionRead
                    | Capability::RelationRead
            ),
            Self::Tester => matches!(
                capability,
                Capability::IssueRead
                    | Capability::CommentRead
                    | Capability::CommentFindMarker
                    | Capability::CommentCreate
                    | Capability::IssueAttachmentUpload
            ),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "admin" => Ok(Self::Admin),
            "orchestrator" => Ok(Self::Orchestrator),
            "executor" => Ok(Self::Executor),
            "reviewer" => Ok(Self::Reviewer),
            "tester" => Ok(Self::Tester),
            _ => Err(format!(
                "invalid role '{value}'; expected admin, orchestrator, executor, reviewer, or tester"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    IssueRead,
    IssueSearch,
    IssueCreate,
    IssueUpdateBody,
    IssueClose,
    IssueAttachmentUpload,
    RepoCreate,
    CommentCreate,
    CommentRead,
    CommentFindMarker,
    ProjectRead,
    ProjectCreate,
    IssueStatusRead,
    VersionRead,
    RelationRead,
    RelationCreate,
    RelationDelete,
}

impl Capability {
    pub const fn description(self) -> &'static str {
        match self {
            Self::IssueRead => "Read one issue",
            Self::IssueSearch => "Search issues",
            Self::IssueCreate => "Create an issue",
            Self::IssueUpdateBody => "Update an issue",
            Self::IssueClose => "Close an issue",
            Self::IssueAttachmentUpload => {
                "Upload an issue attachment (uniformly not-supported; kept for parity and future re-enable)"
            }
            Self::RepoCreate => "Create a private repository",
            Self::CommentCreate => "Create one authorized comment",
            Self::CommentRead => "Read issue comments",
            Self::CommentFindMarker => "Find a comment by marker",
            Self::ProjectRead => "List projects (Redmine, GitLab, or local)",
            Self::ProjectCreate => {
                "Create a project (Redmine or local; Forgejo/GitLab use `repo create`)"
            }
            Self::IssueStatusRead => "List issue statuses (Redmine, GitLab catalogue, or local)",
            Self::VersionRead => "List project versions (Redmine or GitLab milestones)",
            Self::RelationRead => "List issue relations (Redmine or GitLab)",
            Self::RelationCreate => "Create an issue relation (Redmine or GitLab)",
            Self::RelationDelete => "Delete an issue relation (Redmine or GitLab)",
        }
    }

    pub const fn operation(self) -> &'static str {
        match self {
            Self::IssueRead => "issue read",
            Self::IssueSearch => "issue search",
            Self::IssueCreate => "issue create",
            Self::IssueUpdateBody => "issue update",
            Self::IssueClose => "issue close",
            Self::IssueAttachmentUpload => "issue upload-attachment",
            Self::RepoCreate => "repo create",
            Self::CommentCreate => "comment create",
            Self::CommentRead => "comment get",
            Self::CommentFindMarker => "comment find-marker",
            Self::ProjectRead => "project list",
            Self::ProjectCreate => "project create",
            Self::IssueStatusRead => "issue status list",
            Self::VersionRead => "version list",
            Self::RelationRead => "relation list",
            Self::RelationCreate => "relation create",
            Self::RelationDelete => "relation delete",
        }
    }
}
