//! Command/subcommand registry: one descriptor tree for the whole CLI
//! surface. Each node carries its role gate, compile-time feature, and
//! provider scope so the parser, help, and execution gates share a single
//! source instead of drifting apart.
//!
//! Phase 1 skeleton (issue 597): the tree describes the accepted surface
//! exactly as the parser routes it today, but nothing is wired into help or
//! dispatch yet, so user-visible behavior is unchanged. Top-level parser
//! routing already consults [`top_level`], so a command cannot be accepted
//! without a registry entry. Later phases consume [`COMMANDS`] for role-aware
//! help, parser rejection, and MCP surface alignment.

use crate::policy::{Capability, Role};
use crate::providers::ProviderKind;

#[path = "registry_commands.rs"]
mod commands;
#[path = "registry_query.rs"]
mod query;

pub(crate) use commands::COMMANDS;
#[cfg(test)]
pub(crate) use query::top_level_names;
pub(crate) use query::{allows_role, denied_operation};

/// Compile-time optional capability backed by a Cargo feature. The registry
/// records the boundary; later phases decide whether a missing feature hides
/// a command, rejects it, or returns a structured not-compiled result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Feature {
    /// The desktop shell behind `--features gui`.
    Gui,
}

impl Feature {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Gui => "gui",
        }
    }

    /// Whether this build compiled the optional capability in.
    pub(crate) const fn is_compiled(self) -> bool {
        match self {
            Self::Gui => cfg!(feature = "gui"),
        }
    }
}

/// Which role contexts may run a command or subcommand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoleAccess {
    /// Any role, or no role at all.
    Open,
    /// Any role, but a role context is required.
    AnyRole,
    /// No role, or exactly one of the listed roles. Used by the local
    /// branch-context writes (`issue bind`/`issue unbind`), which keep the
    /// historical role-less passthrough but deny explicit child roles.
    RolelessOrOnly(&'static [Role]),
    /// Any role that [`Role::allows`] covers for the capability. A missing
    /// role is rejected.
    Capability(Capability),
    /// Exactly the listed roles; a missing role is rejected.
    Only(&'static [Role]),
    /// `Role::Admin` only (human-operator provisioning). Every AI role is
    /// denied.
    AdminOnly,
}

impl RoleAccess {
    /// Whether an invocation without a role context is accepted.
    pub(crate) const fn allows_roleless(self) -> bool {
        matches!(self, Self::Open | Self::RolelessOrOnly(_))
    }

    /// Whether an explicit role may run the command.
    pub(crate) const fn allows_role(self, role: Role) -> bool {
        match self {
            Self::Open | Self::AnyRole => true,
            Self::RolelessOrOnly(roles) | Self::Only(roles) => role_in_list(role, roles),
            Self::Capability(capability) => role.allows(capability),
            Self::AdminOnly => matches!(role, Role::Admin),
        }
    }

    /// Help visibility for a resolved role context. No role keeps the
    /// compatibility superset view; a resolved role only sees what it may run.
    pub(crate) const fn visible(self, role: Option<Role>) -> bool {
        match role {
            None => true,
            Some(role) => self.allows_role(role),
        }
    }
}

/// Provider the command surface belongs to. Declared on top-level groups only;
/// descendants carry [`ProviderScope::Any`] and rely on their group's scope, so
/// each group keeps its provider condition in one place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderScope {
    /// Visible in the superset view for every provider.
    Any,
    /// Listed only when the resolved provider is Redmine.
    Redmine,
    /// Listed only when the resolved provider is not Redmine.
    NonRedmine,
}

impl ProviderScope {
    pub(crate) const fn visible(self, provider: Option<ProviderKind>) -> bool {
        match self {
            Self::Any => true,
            Self::Redmine => matches!(provider, Some(ProviderKind::Redmine)),
            Self::NonRedmine => !matches!(provider, Some(ProviderKind::Redmine)),
        }
    }
}

/// One node of the command surface. `children` are the subcommands (or nested
/// groups) accepted after `name`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CommandSpec {
    pub(crate) name: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) access: RoleAccess,
    /// Permission `operation` label used by the parser/execution denial
    /// envelope. Empty for capability-gated nodes (the capability supplies
    /// it) and for nodes that can never be denied; command-level gates set
    /// it explicitly so the parser emits the same operation string the
    /// execution layer would.
    pub(crate) operation: &'static str,
    pub(crate) feature: Option<Feature>,
    /// Provider scope of the top-level group this node belongs to; only
    /// top-level nodes carry anything other than [`ProviderScope::Any`].
    pub(crate) provider_scope: ProviderScope,
    pub(crate) children: &'static [CommandSpec],
}

impl CommandSpec {
    /// The `operation` field of the stable permission denial envelope.
    pub(crate) fn operation(&self) -> &'static str {
        if self.operation.is_empty() {
            match self.access {
                RoleAccess::Capability(capability) => capability.operation(),
                _ => self.name,
            }
        } else {
            self.operation
        }
    }

    /// Whether `role` may run this node. A group requires both its own access
    /// and at least one runnable child.
    pub(crate) const fn allows_role(&self, role: Role) -> bool {
        if !self.access.allows_role(role) {
            return false;
        }
        if self.children.is_empty() {
            return true;
        }
        any_child_allows(self.children, role)
    }

    /// Help visibility for a role context; no role keeps the superset view.
    pub(crate) const fn visible_for(&self, role: Option<Role>) -> bool {
        match role {
            None => true,
            Some(role) => self.allows_role(role),
        }
    }
}

/// A leaf command with no subcommands and no provider override.
pub(super) const fn leaf(
    name: &'static str,
    summary: &'static str,
    access: RoleAccess,
) -> CommandSpec {
    CommandSpec {
        name,
        summary,
        access,
        operation: "",
        feature: None,
        provider_scope: ProviderScope::Any,
        children: &[],
    }
}

/// A leaf command whose command-level gate needs an explicit permission
/// operation label (non-capability gates such as `Only`/`AdminOnly`).
pub(super) const fn leaf_op(
    name: &'static str,
    summary: &'static str,
    access: RoleAccess,
    operation: &'static str,
) -> CommandSpec {
    CommandSpec {
        name,
        summary,
        access,
        operation,
        feature: None,
        provider_scope: ProviderScope::Any,
        children: &[],
    }
}

/// A group with subcommands.
pub(super) const fn group(
    name: &'static str,
    summary: &'static str,
    access: RoleAccess,
    provider_scope: ProviderScope,
    children: &'static [CommandSpec],
) -> CommandSpec {
    CommandSpec {
        name,
        summary,
        access,
        operation: "",
        feature: None,
        provider_scope,
        children,
    }
}

const ORCHESTRATOR_ONLY: &[Role] = &[Role::Orchestrator];
const WORKTREE_READ_ROLES: &[Role] = &[Role::Orchestrator, Role::Executor, Role::Reviewer];
const NOTIFY_ROLES: &[Role] = &[
    Role::Orchestrator,
    Role::Executor,
    Role::Reviewer,
    Role::Tester,
];

const ACCESS_ISSUE_READ: RoleAccess = RoleAccess::Capability(Capability::IssueRead);
const ACCESS_ISSUE_SEARCH: RoleAccess = RoleAccess::Capability(Capability::IssueSearch);
const ACCESS_ISSUE_CREATE: RoleAccess = RoleAccess::Capability(Capability::IssueCreate);
const ACCESS_ISSUE_UPDATE: RoleAccess = RoleAccess::Capability(Capability::IssueUpdateBody);
const ACCESS_ISSUE_CLOSE: RoleAccess = RoleAccess::Capability(Capability::IssueClose);
const ACCESS_ATTACHMENT: RoleAccess = RoleAccess::Capability(Capability::IssueAttachmentUpload);
const ACCESS_COMMENT_CREATE: RoleAccess = RoleAccess::Capability(Capability::CommentCreate);
const ACCESS_COMMENT_READ: RoleAccess = RoleAccess::Capability(Capability::CommentRead);
const ACCESS_FIND_MARKER: RoleAccess = RoleAccess::Capability(Capability::CommentFindMarker);
const ACCESS_PROJECT_READ: RoleAccess = RoleAccess::Capability(Capability::ProjectRead);
const ACCESS_PROJECT_CREATE: RoleAccess = RoleAccess::Capability(Capability::ProjectCreate);
const ACCESS_STATUS_READ: RoleAccess = RoleAccess::Capability(Capability::IssueStatusRead);
const ACCESS_VERSION_READ: RoleAccess = RoleAccess::Capability(Capability::VersionRead);
const ACCESS_RELATION_READ: RoleAccess = RoleAccess::Capability(Capability::RelationRead);
const ACCESS_RELATION_CREATE: RoleAccess = RoleAccess::Capability(Capability::RelationCreate);
const ACCESS_RELATION_DELETE: RoleAccess = RoleAccess::Capability(Capability::RelationDelete);
const ACCESS_REPO_CREATE: RoleAccess = RoleAccess::Capability(Capability::RepoCreate);
const ACCESS_ORCHESTRATOR: RoleAccess = RoleAccess::Only(ORCHESTRATOR_ONLY);
const ACCESS_WORKTREE_READ: RoleAccess = RoleAccess::Only(WORKTREE_READ_ROLES);
const ACCESS_BIND: RoleAccess = RoleAccess::RolelessOrOnly(ORCHESTRATOR_ONLY);
const ACCESS_NOTIFY: RoleAccess = RoleAccess::Only(NOTIFY_ROLES);

const fn any_child_allows(children: &[CommandSpec], role: Role) -> bool {
    let mut index = 0;
    while index < children.len() {
        if children[index].allows_role(role) {
            return true;
        }
        index += 1;
    }
    false
}

const fn role_in_list(role: Role, roles: &[Role]) -> bool {
    let mut index = 0;
    while index < roles.len() {
        if roles[index] as u8 == role as u8 {
            return true;
        }
        index += 1;
    }
    false
}

/// Resolve a top-level command name. This is the shared source of truth for
/// parser routing, so a name can never be accepted without a registry entry.
pub(crate) fn top_level(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|spec| spec.name == name)
}

/// Resolve a command path such as `["worktree", "status"]`.
pub(crate) fn find(path: &[&str]) -> Option<&'static CommandSpec> {
    let (head, rest) = path.split_first()?;
    let mut node = top_level(head)?;
    for name in rest {
        node = node.children.iter().find(|spec| spec.name == *name)?;
    }
    Some(node)
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
