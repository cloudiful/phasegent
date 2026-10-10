//! Role and capability policy. The five roles (`admin`, `orchestrator`,
//! `executor`, `reviewer`, `explore`) gate every CLI primitive and the
//! `Capability` enum is the closed set of operations the CLI
//! exposes to roles and providers.

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Admin,
    Orchestrator,
    Executor,
    Reviewer,
    /// Read-only reconnaissance role. It carries issue read in this
    /// policy; its structured-record surface is a command-level
    /// registry gate because a record write is also bound to one
    /// record kind (see `command::registry`), which no single
    /// capability row can express.
    Explore,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Orchestrator => "orchestrator",
            Self::Executor => "executor",
            Self::Reviewer => "reviewer",
            Self::Explore => "explore",
        }
    }

    /// The roles that may read a structured record (`record get`/`record list`).
    ///
    /// Declared next to the role policy so the command registry and the
    /// execution gate share one list; `admin` is deliberately absent
    /// because it is the human bootstrap role and never an agent.
    pub const RECORD_READ_ROLES: &'static [Role] = &[
        Self::Orchestrator,
        Self::Executor,
        Self::Reviewer,
        Self::Explore,
    ];

    /// The roles that may create a structured record. Equal to
    /// [`Role::RECORD_READ_ROLES`]; the narrower per-kind and
    /// `--authorized` restrictions are enforced by the record command
    /// itself, not by widening the set of write-capable roles.
    pub const RECORD_WRITE_ROLES: &'static [Role] = Self::RECORD_READ_ROLES;

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
                    | Capability::Notify
            ),
            // The reviewer is the single independent verification role: it
            // audits the code, verifies the acceptance criteria, and runs the
            // tests, so it carries the attachment surface the test-only write
            // boundary needs.
            Self::Reviewer => matches!(
                capability,
                Capability::IssueRead
                    | Capability::CommentRead
                    | Capability::CommentFindMarker
                    | Capability::CommentCreate
                    | Capability::IssueAttachmentUpload
                    | Capability::ProjectRead
                    | Capability::IssueStatusRead
                    | Capability::VersionRead
                    | Capability::RelationRead
                    | Capability::Notify
            ),
            // Reconnaissance is read-only: it reads issues and nothing else.
            // No comment create, no notify, no attachment, no metadata. The
            // one write it may ever perform is an authorized `recon` record,
            // which the `record` command gate admits separately.
            Self::Explore => matches!(capability, Capability::IssueRead),
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
            "explore" => Ok(Self::Explore),
            _ => Err(format!(
                "invalid role '{value}'; expected admin, orchestrator, executor, reviewer, or explore"
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
    CommentCreate,
    CommentRead,
    CommentFindMarker,
    /// Deliver one bounded manual agent notification (`notify send`). Not a
    /// provider operation: every command path gates on it. Admin is
    /// excluded because it only bootstraps.
    Notify,
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
            Self::CommentCreate => "Create one authorized comment",
            Self::CommentRead => "Read issue comments",
            Self::CommentFindMarker => "Find a comment by marker",
            Self::Notify => "Send one bounded agent notification",
            Self::ProjectRead => "List projects (Redmine or local)",
            Self::ProjectCreate => "Create a project (Redmine or local)",
            Self::IssueStatusRead => "List issue statuses (Redmine or local)",
            Self::VersionRead => "List project versions (Redmine)",
            Self::RelationRead => "List issue relations (Redmine)",
            Self::RelationCreate => "Create an issue relation (Redmine)",
            Self::RelationDelete => "Delete an issue relation (Redmine)",
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
            Self::CommentCreate => "comment create",
            Self::CommentRead => "comment get",
            Self::CommentFindMarker => "comment find-marker",
            Self::Notify => "notify send",
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
