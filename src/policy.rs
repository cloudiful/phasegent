use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Admin,
    Orchestrator,
    Executor,
    Reviewer,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Orchestrator => "orchestrator",
            Self::Executor => "executor",
            Self::Reviewer => "reviewer",
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
            _ => Err(format!(
                "invalid role '{value}'; expected admin, orchestrator, executor, or reviewer"
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
            Self::IssueUpdateBody => "Update an issue body",
            Self::IssueClose => "Close an issue",
            Self::RepoCreate => "Create a private repository",
            Self::CommentCreate => "Create one authorized comment",
            Self::CommentRead => "Read issue comments",
            Self::CommentFindMarker => "Find a comment by marker",
            Self::ProjectRead => "List Redmine projects",
            Self::ProjectCreate => "Create a Redmine project",
            Self::IssueStatusRead => "List Redmine issue statuses",
            Self::VersionRead => "List Redmine project versions",
            Self::RelationRead => "List Redmine or GitLab issue relations",
            Self::RelationCreate => "Create a Redmine or GitLab issue relation",
            Self::RelationDelete => "Delete a Redmine or GitLab issue relation",
        }
    }

    pub const fn operation(self) -> &'static str {
        match self {
            Self::IssueRead => "issue read",
            Self::IssueSearch => "issue search",
            Self::IssueCreate => "issue create",
            Self::IssueUpdateBody => "issue update-body",
            Self::IssueClose => "issue close",
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
