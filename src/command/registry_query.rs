//! Role/path queries over the command registry. Split from `registry.rs` so the
//! descriptor types stay in one focused module while the parser and help share
//! these lookups as the single source of truth for role gating.

use super::{COMMANDS, Role, find};

/// Whether `role` may run the command at `path`. An unknown path is denied,
/// so a command the registry cannot resolve is never silently accepted.
pub(crate) fn allows_role(role: Role, path: &[&str]) -> bool {
    find(path).is_some_and(|spec| spec.allows_role(role))
}

/// The permission `operation` label for a role-denied `path`, or `None` when
/// the path is unknown or the role is allowed.
pub(crate) fn denied_operation(role: Role, path: &[&str]) -> Option<&'static str> {
    let spec = find(path)?;
    if spec.allows_role(role) {
        None
    } else {
        Some(spec.operation())
    }
}

/// Every registered top-level command name, in declaration order.
pub(crate) fn top_level_names() -> Vec<&'static str> {
    COMMANDS.iter().map(|spec| spec.name).collect()
}
