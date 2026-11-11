//! Descriptor table (part 2): the operator/plugin groups in declaration
//! order, from `worktree` through `notify`. Spliced into `super::COMMANDS` after
//! `core::CORE` by name, so command order and every role/provider gate stay
//! exactly as declared.

use super::super::{
    ACCESS_NOTIFY, ACCESS_ORCHESTRATOR, ACCESS_WORKTREE_READ, CommandSpec, ProviderScope,
    RoleAccess, group, leaf, leaf_op,
};

pub(super) const OPS: &[CommandSpec] = &[
    group(
        "worktree",
        "Registered worktree directories (acquire/prune orchestrator-only; status/list readable by orchestrator, executor, reviewer)",
        RoleAccess::Open,
        ProviderScope::Any,
        &[
            leaf_op(
                "acquire",
                "Acquire or reuse a worktree directory",
                ACCESS_ORCHESTRATOR,
                "worktree acquire",
            ),
            leaf_op(
                "status",
                "Show directories associated with an issue",
                ACCESS_WORKTREE_READ,
                "worktree status",
            ),
            leaf_op(
                "list",
                "List directories for a repository",
                ACCESS_WORKTREE_READ,
                "worktree list",
            ),
            leaf_op(
                "probe",
                "Read-only directory diagnostic",
                ACCESS_WORKTREE_READ,
                "worktree probe",
            ),
            leaf_op(
                "prune",
                "Prune registered worktrees",
                ACCESS_ORCHESTRATOR,
                "worktree prune",
            ),
        ],
    ),
    group(
        "hooks",
        "Managed Git hook installation",
        RoleAccess::Open,
        ProviderScope::Any,
        &[
            leaf("install", "Install managed Git hooks", RoleAccess::Open),
            leaf("run", "Internal hook entry point", RoleAccess::Open),
        ],
    ),
    group(
        "plugin",
        "Managed OpenCode plugin installation",
        RoleAccess::Open,
        ProviderScope::Any,
        &[
            leaf("install", "Install the OpenCode plugin", RoleAccess::Open),
            leaf("status", "Report plugin state", RoleAccess::Open),
            leaf("uninstall", "Remove the OpenCode plugin", RoleAccess::Open),
        ],
    ),
    group(
        "skill",
        "Install the bundled phasegent skill directory",
        RoleAccess::Open,
        ProviderScope::Any,
        &[leaf(
            "install",
            "Install the phasegent skill and references",
            RoleAccess::Open,
        )],
    ),
    group(
        "notify",
        "Bounded agent notifications",
        RoleAccess::Open,
        ProviderScope::Any,
        &[leaf_op(
            "send",
            "Deliver one bounded notification",
            ACCESS_NOTIFY,
            "notify send",
        )],
    ),
];
