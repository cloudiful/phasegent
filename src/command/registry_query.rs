//! Compile-time feature boundary and role/path queries over the command
//! registry. Split from `registry.rs` so the descriptor tree stays in one
//! focused module while the feature boundary, role gating, and the parser and
//! help share these lookups as a single source of truth.

use super::{COMMANDS, Role, find};

/// Compile-time optional capability backed by a Cargo feature. The registry
/// records the boundary; [`is_compiled`](Feature::is_compiled) decides whether
/// the build has it, visibility hides it when absent, and
/// [`not_compiled_message`](Feature::not_compiled_message) explains it.
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

    /// Stable explanation for a build that did not compile this capability.
    /// The execution-layer stub prints the same text inside its structured
    /// `kind: "gui"` error, so help and runtime share one not-compiled
    /// contract.
    pub(crate) const fn not_compiled_message(self) -> &'static str {
        match self {
            Self::Gui => {
                "GUI support was not compiled into this binary; rebuild with --features gui to enable the desktop shell"
            }
        }
    }
}

/// Why a registered path is unavailable in this build and role context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Unavailable {
    /// The node requires a compile-time feature this binary does not have.
    NotCompiled(Feature),
    /// The resolved role is denied the node's permission operation.
    RoleDenied { role: Role, operation: &'static str },
}

/// Whether `role` may run the command at `path`. An unknown path is denied, so
/// a command the registry cannot resolve is never silently accepted, and a
/// node whose feature this binary did not compile is denied because nothing
/// can run what is absent.
pub(crate) fn allows_role(role: Role, path: &[&str]) -> bool {
    find(path).is_some_and(|spec| spec.is_compiled() && spec.allows_role(role))
}

/// The stable reason a registered path is unavailable, or `None` when it is
/// available or unknown. The compile-time feature is checked first so a
/// missing feature never masquerades as a role denial; a missing role keeps
/// the compatibility superset view for compiled commands.
pub(crate) fn unavailability(role: Option<Role>, path: &[&str]) -> Option<Unavailable> {
    let spec = find(path)?;
    if let Some(feature) = spec.feature
        && !feature.is_compiled()
    {
        return Some(Unavailable::NotCompiled(feature));
    }
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
/// the path is unknown, allowed, or not compiled. The parser keeps accepting a
/// not-compiled path so the execution layer can answer with the structured
/// not-compiled error instead of a permission denial.
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
