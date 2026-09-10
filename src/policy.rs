//! Role and capability policy. The five roles (`admin`, `orchestrator`,
//! `executor`, `reviewer`, `tester`) gate every CLI/MCP primitive and
//! the `Capability` enum is the closed set of operations the CLI
//! exposes to roles and providers.
//!
//! # Issue 257 — GitLab/Redmine parity matrix
//!
//! This module is the documentation anchor for the GitLab↔Redmine
//! capability parity tracked in issue 257. The matrix below is the
//! contract every provider dispatcher and CLI guard relies on; the
//! underlying provider implementations and dispatcher arm forwards
//! in `src/providers/dispatch/issue.rs` stay the source of truth for
//! the live `supports()` result.
//!
//! | Capability             | Forgejo | Redmine | GitLab | Local |
//! |------------------------|:-------:|:-------:|:------:|:-----:|
//! | IssueRead              |   yes   |   yes   |  yes   |  yes  |
//! | IssueSearch            |   yes   |   yes   |  yes   |  yes  |
//! | IssueCreate            |   yes   |   yes   |  yes   |  yes  |
//! | IssueUpdateBody        |   yes   |   yes   |  yes   |  yes  |
//! | IssueClose             |   yes   |   yes   |  yes   |  yes  |
//! | IssueAttachmentUpload  |   no    | **no**  |  no    |  no   |
//! | CommentCreate          |   yes   |   yes   |  yes   |  yes  |
//! | CommentRead            |   yes   |   yes   |  yes   |  yes  |
//! | CommentFindMarker      |   yes   |   yes   |  yes   |  yes  |
//! | RepoCreate             |   yes   |   no    |  yes   |  no   |
//! | ProjectRead            |   no    |   yes   |  yes   |  yes  |
//! | ProjectCreate          |   no    |   yes   |  no    |  yes  |
//! | IssueStatusRead        |   no    |   yes   |  yes   |  yes  |
//! | VersionRead            |   no    |   yes   |  yes   |  yes  |
//! | RelationRead           |   no    |   yes   |  yes   |  no   |
//! | RelationCreate         |   no    |   yes   |  yes   |  no   |
//! | RelationDelete         |   no    |   yes   |  yes   |  no   |
//!
//! **IssueAttachmentUpload** is the only row where the matrix unifies
//! the GitLab↔Redmine pair (both sides report not-supported so the
//! CLI/MCP layer rejects every provider uniformly; evidence moves to
//! comments / external links). Phase 4 sank the value onto the
//! inherent provider's `supports` so the dispatcher arm is a thin
//! forwarder and no longer carries a separate override.
//!
//! **Phase 2** filled the read-side parity rows that originally
//! stayed `no` for GitLab with equivalent reads:
//!   * `ProjectRead` → `GET /projects` (mapped onto `RedmineProject`).
//!   * `IssueStatusRead` → static `WORKFLOW_LABELS` catalogue
//!     (mapped onto `RedmineIssueStatus`; GitLab has no native status
//!     enum).
//!   * `VersionRead` → `GET /projects/:id/milestones` (mapped onto
//!     `RedmineVersion`).
//!
//! `ProjectCreate` stays `no` for GitLab because the equivalent lives
//! on the `repo create` path (`POST /projects` via
//! `RepoProvider::create_repo`); there is intentionally only one
//! entry point to that endpoint.
//!
//! **Phase 3** added `relates` relation auto on the `issue create`
//! CLI path when `--parent-issue` is provided (Redmine and GitLab).
//! The helper is idempotent and reports a bounded warning on failure
//! without polluting stdout or exit code. Forgejo and Local have no
//! relation surface, so they stay `no` on every relation row.
//!
//! The DTO widening (Phase 2 — `ApiIssue` now decodes `milestone`,
//! `due_date`, `weight`, `time_stats`, `assignee(s)`, `created_at`,
//! `updated_at`) keeps the old field shape required so existing
//! fixtures and audit-comment consumers stay compatible. Planning
//! flags are accepted as CLI input but accepted differently per
//! provider: Redmine forwards them as native fields (parent issue,
//! fixed version, dates, estimated hours, done ratio), GitLab accepts
//! `--estimated-hours` via the native time_estimate endpoint and maps
//! `--tracker` to a `type::*` label, Forgejo rejects every planning
//! flag, and Local accepts them but does not persist (the local index
//! only stores title/body/state).

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
            Self::IssueUpdateBody => "Update an issue body",
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
            Self::IssueUpdateBody => "issue update-body",
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
