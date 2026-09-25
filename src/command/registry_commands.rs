//! Descriptor table for the command registry: every top-level command and
//! subcommand the parser accepts, with its role gate, feature, and provider
//! scope. Gate constants and types live in [`super`]; lookup goes through
//! [`super::find`] / [`super::top_level`]. The `summary` text is the row
//! rendered by role-specific root help; command-level gates carry their
//! permission `operation` label so parser denials name the same operation the
//! execution layer would.
//!
//! The tree is split across two sibling files to stay under the repository's
//! file-size planning threshold: `core::CORE` holds the workflow groups and
//! `ops::OPS` the operator/plugin/MCP groups. [`COMMANDS`] concatenates them
//! in declaration order, so the combined surface and every gate are unchanged.

#[path = "registry_commands_core.rs"]
mod core;
#[path = "registry_commands_ops.rs"]
mod ops;

use super::{CommandSpec, RoleAccess, leaf};

const TOTAL: usize = core::CORE.len() + ops::OPS.len();

/// Scratch element only used to size the concatenation buffer; every slot is
/// overwritten before the table is exposed.
const PLACEHOLDER: CommandSpec = leaf("", "", RoleAccess::Open);

/// Compile-time concatenation of the ordered descriptor chunks.
const fn concat() -> [CommandSpec; TOTAL] {
    let parts: [&[CommandSpec]; 2] = [core::CORE, ops::OPS];
    let mut out = [PLACEHOLDER; TOTAL];
    let mut part = 0;
    let mut index = 0;
    while part < parts.len() {
        let slice = parts[part];
        let mut offset = 0;
        while offset < slice.len() {
            out[index] = slice[offset];
            index += 1;
            offset += 1;
        }
        part += 1;
    }
    out
}

static COMMANDS_TABLE: [CommandSpec; TOTAL] = concat();

/// The whole accepted command surface, top-level commands first.
pub(crate) static COMMANDS: &[CommandSpec] = &COMMANDS_TABLE;

#[cfg(test)]
mod tests {
    use super::COMMANDS;

    /// The tree is declared in two chunks; concatenation must keep the exact
    /// declaration order and must not drop or duplicate a top-level command.
    #[test]
    fn split_chunks_concatenate_in_declaration_order() {
        let names: Vec<&str> = COMMANDS.iter().map(|spec| spec.name).collect();
        assert_eq!(
            names,
            [
                "gui", "doctor", "admin", "auth", "config", "issue", "comment", "project",
                "status", "version", "relation", "timer", "workflow", "worktree", "repo", "hooks",
                "plugin", "notify", "mcp",
            ]
        );
    }
}
