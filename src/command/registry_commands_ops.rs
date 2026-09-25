//! Descriptor table (part 2): the operator/plugin/MCP groups in declaration
//! order, from `worktree` through `mcp`. Spliced into `super::COMMANDS` after
//! `core::CORE` by name, so command order and every role/provider gate stay
//! exactly as declared.

use super::super::{
    ACCESS_NOTIFY, ACCESS_ORCHESTRATOR, ACCESS_REPO_CREATE, ACCESS_WORKTREE_READ, CommandSpec,
    ProviderScope, RoleAccess, group, leaf, leaf_op,
};

pub(super) const OPS: &[CommandSpec] = &[
    group(
        "worktree",
        "Local per-(repo, issue, session) worktree leases (acquire/release/prune orchestrator-only; status/list readable by orchestrator, executor, reviewer)",
        RoleAccess::Open,
        ProviderScope::Any,
        &[
            leaf_op(
                "acquire",
                "Acquire a worktree lease",
                ACCESS_ORCHESTRATOR,
                "worktree acquire",
            ),
            leaf_op(
                "release",
                "Release a worktree lease",
                ACCESS_ORCHESTRATOR,
                "worktree release",
            ),
            leaf_op(
                "status",
                "Show a lease by issue",
                ACCESS_WORKTREE_READ,
                "worktree status",
            ),
            leaf_op(
                "list",
                "List leases for a repository",
                ACCESS_WORKTREE_READ,
                "worktree list",
            ),
            leaf_op(
                "probe",
                "Read-only lease diagnostic",
                ACCESS_WORKTREE_READ,
                "worktree probe",
            ),
            leaf_op(
                "prune",
                "Prune stale leases",
                ACCESS_ORCHESTRATOR,
                "worktree prune",
            ),
            leaf_op(
                "heartbeat",
                "Refresh a lease heartbeat",
                ACCESS_ORCHESTRATOR,
                "worktree heartbeat",
            ),
        ],
    ),
    group(
        "repo",
        "Repository operations",
        RoleAccess::Open,
        ProviderScope::NonRedmine,
        &[leaf(
            "create",
            "Create a private repository",
            ACCESS_REPO_CREATE,
        )],
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
    group(
        "mcp",
        "MCP server over stdio or streamable HTTP",
        RoleAccess::Open,
        ProviderScope::Any,
        &[leaf_op(
            "serve",
            "Serve the contracted MCP tools",
            RoleAccess::AnyRole,
            "mcp serve",
        )],
    ),
];
