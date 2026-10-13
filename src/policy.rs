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

/// Invocation policy context: unmanaged human Operator vs managed Agent roles.
///
/// The environment signals are routing/policy, not authenticated identity:
/// without `PHASEGENT_ROLE` and without `PHASEGENT_SESSION_ID` the CLI is an
/// unmanaged Operator; an explicit valid role keeps the existing agent-role
/// permission matrix; with `PHASEGENT_SESSION_ID` present (even blank) but no
/// role the invocation is a managed session missing its role and fails closed
/// for business commands. Invalid roles always error and never downgrade.
///
/// Operator is invocation context, never a new agent role or persisted
/// identity: it authorizes all ordinary business and administrative commands
/// (using the existing admin credential centrally) except the agent-authored
/// `record create`, which stays role/kind-bound. The five `Role` values,
/// credential storage, and `Role::allows` matrix are unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallerContext {
    /// Unmanaged human operator: no role and no managed session marker.
    Operator,
    /// Explicit agent role (managed or explicit unmanaged); gated by
    /// [`Role::allows`] exactly as before.
    Agent(Role),
    /// Managed session marker present but no role: fails closed for
    /// business/permissioned commands, utility allowlist stays available.
    ManagedWithoutRole,
}

/// Environment variable that marks a managed session. Presence (even blank)
/// matters, not the value.
pub const SESSION_ENV: &str = "PHASEGENT_SESSION_ID";
/// Environment variable that carries the explicit agent role.
#[allow(dead_code)]
pub const ROLE_ENV: &str = "PHASEGENT_ROLE";

/// Whether a managed session marker is present (even blank). Blank counts as
/// present so an empty inheritance still fails closed instead of silently
/// becoming an Operator. Presence is determined with `var_os` so a present
/// Unix value that is not valid Unicode still counts as present (including
/// blank and non-Unicode values) and fails closed instead of downgrading.
pub fn session_present() -> bool {
    std::env::var_os(SESSION_ENV).is_some()
}

/// Resolve `PHASEGENT_ROLE` from the process environment with Unix raw-value
/// semantics: absent (`NotPresent`) or blank means no explicit role;
/// present non-Unicode (`NotUnicode`) is an argument error and is never
/// treated as absent; otherwise the trimmed value must parse as a [`Role`].
/// Central helper for the production parse path so both the parser and
/// [`CallerContext`] detection obey the same rule before any permissioned
/// side effect. Pure parser tests stay deterministic by injecting `&str`.
pub fn role_from_env() -> Result<Option<Role>, String> {
    match std::env::var_os(ROLE_ENV) {
        None => Ok(None),
        Some(raw) => match raw.to_str() {
            None => Err("PHASEGENT_ROLE is invalid: value is not valid Unicode".to_owned()),
            Some(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    trimmed
                        .parse::<Role>()
                        .map(Some)
                        .map_err(|error| format!("PHASEGENT_ROLE is invalid: {error}"))
                }
            }
        },
    }
}

/// Whether `session_env` as injected by tests marks a managed session.
/// `None` means absent; `Some(_)` (even empty) means present.
#[allow(dead_code)]
pub fn session_present_for(session_env: Option<&str>) -> bool {
    session_env.is_some()
}

impl CallerContext {
    /// Build from an explicit role and real-process session presence.
    pub fn from_role(role: Option<Role>) -> Self {
        Self::from_role_and_session(role, session_present())
    }

    /// Test-injectable constructor: `session_env` models `PHASEGENT_SESSION_ID`
    /// presence (`None` absent, `Some(_)` present even when blank).
    pub fn from_role_and_session(role: Option<Role>, session_present: bool) -> Self {
        match (role, session_present) {
            (Some(role), _) => Self::Agent(role),
            (None, false) => Self::Operator,
            (None, true) => Self::ManagedWithoutRole,
        }
    }

    /// The explicit agent role, if any. Used for role-filtered help and
    /// scoped config semantics; `None` for Operator and managed-missing.
    #[allow(dead_code)]
    pub const fn role(self) -> Option<Role> {
        match self {
            Self::Agent(role) => Some(role),
            Self::Operator | Self::ManagedWithoutRole => None,
        }
    }

    pub const fn is_operator(self) -> bool {
        matches!(self, Self::Operator)
    }

    pub const fn is_managed_without_role(self) -> bool {
        matches!(self, Self::ManagedWithoutRole)
    }

    /// Central credential routing, distinct from authorization: Operator
    /// resolves to the existing admin credential/settings; an Agent resolves
    /// to its own role; a managed session without a role has no credential.
    /// No fallback across keys.
    pub const fn credential_role(self) -> Option<Role> {
        match self {
            Self::Operator => Some(Role::Admin),
            Self::Agent(role) => Some(role),
            Self::ManagedWithoutRole => None,
        }
    }

    /// Whether `self` authorizes `capability`. Operator authorizes every
    /// capability (full ordinary/admin authority is not implemented by
    /// widening `Role::Admin::allows`); Agent delegates to `Role::allows`;
    /// managed-without-role authorizes nothing.
    pub fn allows(self, capability: Capability) -> bool {
        match self {
            Self::Operator => true,
            Self::Agent(role) => role.allows(capability),
            Self::ManagedWithoutRole => false,
        }
    }

    /// Whether `self` is orchestrator-authorized (orchestrator agent or
    /// unmanaged Operator). Used by the orchestrator-only lifecycle gates
    /// (status transitions, timer, worktree mutating paths, issue sync).
    pub fn is_orchestrator_authorized(self) -> bool {
        match self {
            Self::Operator => true,
            Self::Agent(role) => role == Role::Orchestrator,
            Self::ManagedWithoutRole => false,
        }
    }

    /// Whether `self` may run the admin provisioning surface
    /// (`admin workflow bootstrap` and the explicit admin group): explicit
    /// `Admin` agent or unmanaged Operator. Never widened via `Role::allows`.
    pub fn is_admin_authorized(self) -> bool {
        match self {
            Self::Operator => true,
            Self::Agent(role) => role == Role::Admin,
            Self::ManagedWithoutRole => false,
        }
    }

    /// Whether `self` may read worktree leases (orchestrator/executor/
    /// reviewer surface, plus Operator).
    pub fn is_worktree_read_authorized(self) -> bool {
        match self {
            Self::Operator => true,
            Self::Agent(role) => {
                matches!(role, Role::Orchestrator | Role::Executor | Role::Reviewer)
            }
            Self::ManagedWithoutRole => false,
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
