//! Compile-time capability boundary and role/path queries over the command
//! registry. Split from `registry.rs` so the descriptor tree stays in one
//! focused module while the role gating and the parser and help share these
//! lookups as a single source of truth.

use super::{COMMANDS, Role, find};

/// Compile-time optional capability backed by a Cargo feature.
///
/// No registered command is feature-gated today, so the enum has no variants;
/// [`super::CommandSpec`] keeps the field as the single extension point should
/// a future optional surface need a Cargo feature gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Feature {}

impl Feature {
    /// Whether this build compiled the optional capability in. No variant
    /// exists, so this is only callable for a value that cannot be built.
    pub(crate) const fn is_compiled(self) -> bool {
        match self {}
    }
}

/// Why a registered path is unavailable in this build and role context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Unavailable {
    /// The resolved role is denied the node's permission operation.
    RoleDenied { role: Role, operation: &'static str },
}

/// Whether `role` may run the command at `path`. An unknown path is denied, so
/// a command the registry cannot resolve is never silently accepted.
pub(crate) fn allows_role(role: Role, path: &[&str]) -> bool {
    find(path).is_some_and(|spec| spec.allows_role(role))
}

/// The stable reason a registered path is unavailable, or `None` when it is
/// available or unknown. A missing role keeps the compatibility superset view
/// for every registered command.
pub(crate) fn unavailability(role: Option<Role>, path: &[&str]) -> Option<Unavailable> {
    let spec = find(path)?;
    let role = role?;
    if spec.allows_role(role) {
        return None;
    }
    Some(Unavailable::RoleDenied {
        role,
        operation: spec.operation(),
    })
}

/// The permission `operation` label for a role-denied `path`, or `None` when
/// the path is unknown or allowed.
pub(crate) fn denied_operation(role: Role, path: &[&str]) -> Option<&'static str> {
    match unavailability(Some(role), path) {
        Some(Unavailable::RoleDenied { operation, .. }) => Some(operation),
        _ => None,
    }
}

/// Every registered top-level command name, in declaration order.
pub(crate) fn top_level_names() -> Vec<&'static str> {
    COMMANDS.iter().map(|spec| spec.name).collect()
}
