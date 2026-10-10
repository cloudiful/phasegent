//! Descriptor table for the structured-record group.
//!
//! Kept in its own sibling file because `registry_commands_core.rs`
//! already owns every other workflow group and the repository treats that
//! file as near its planning threshold.
//!
//! The chunk is spliced into `super::COMMANDS` between the workflow groups
//! and the operator groups, so `record` follows `workflow` in declaration
//! order. That order is internal: no rendered view depends on it (root help
//! pins its own overview order), and every role gate travels with the
//! descriptor, so placement cannot widen or narrow access.
//!
//! The group is gated by role, not by a single capability: publishing a
//! record is additionally bound to one record kind, which `Capability`
//! cannot express. The role lists come from `policy.rs`, and the
//! kind/actor binding plus the `--authorized` requirement are enforced by
//! the `record` command itself.

use super::super::{
    ACCESS_RECORD_CREATE, ACCESS_RECORD_READ, CommandSpec, ProviderScope, RoleAccess,
};
use super::super::{group, leaf_op};

pub(super) const RECORD: &[CommandSpec] = &[group(
    "record",
    "Structured agent records: CLI-owned metadata header with a plain note body",
    RoleAccess::Open,
    ProviderScope::Any,
    &[
        leaf_op(
            "create",
            "Publish one authorized record for the session role",
            ACCESS_RECORD_CREATE,
            "record create",
        ),
        leaf_op(
            "get",
            "Read one record by its native reference id",
            ACCESS_RECORD_READ,
            "record get",
        ),
        leaf_op(
            "list",
            "List records on an issue, optionally filtered",
            ACCESS_RECORD_READ,
            "record list",
        ),
    ],
)];
