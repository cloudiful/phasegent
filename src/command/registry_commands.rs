//! Descriptor table for the command registry: every top-level command and
//! subcommand the parser accepts, with its role gate, feature, and provider
//! scope. Gate constants and types live in [`super`]; lookup goes through
//! [`super::find`] / [`super::top_level`].

use super::{
    ACCESS_ATTACHMENT, ACCESS_BIND, ACCESS_COMMENT_CREATE, ACCESS_COMMENT_READ, ACCESS_FIND_MARKER,
    ACCESS_ISSUE_CLOSE, ACCESS_ISSUE_CREATE, ACCESS_ISSUE_READ, ACCESS_ISSUE_SEARCH,
    ACCESS_ISSUE_UPDATE, ACCESS_NOTIFY, ACCESS_ORCHESTRATOR, ACCESS_PROJECT_CREATE,
    ACCESS_PROJECT_READ, ACCESS_RELATION_CREATE, ACCESS_RELATION_DELETE, ACCESS_RELATION_READ,
    ACCESS_REPO_CREATE, ACCESS_STATUS_READ, ACCESS_VERSION_READ, ACCESS_WORKTREE_READ, CommandSpec,
    Feature, ProviderScope, RoleAccess, group, leaf,
};

/// The whole accepted command surface, top-level commands first.
pub(crate) static COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "gui",
        summary: "Open the desktop GUI",
        access: RoleAccess::Open,
        feature: Some(Feature::Gui),
        provider_scope: ProviderScope::Any,
        children: &[],
    },
    leaf("doctor", "Read-only self-check", RoleAccess::Open),
    group(
        "admin",
        "Human-operator provisioning",
        RoleAccess::AdminOnly,
        ProviderScope::Any,
        &[
            group(
                "auth",
                "Store a provider credential",
                RoleAccess::AdminOnly,
                ProviderScope::Any,
                &[leaf(
                    "setup",
                    "Store the per-role credential",
                    RoleAccess::AdminOnly,
                )],
            ),
            group(
                "config",
                "Persist settings and provider",
                RoleAccess::AdminOnly,
                ProviderScope::Any,
                &[
                    leaf("set", "Persist a setting", RoleAccess::AdminOnly),
                    leaf("clear", "Remove a persisted setting", RoleAccess::AdminOnly),
                    group(
                        "provider",
                        "Machine-wide default provider",
                        RoleAccess::AdminOnly,
                        ProviderScope::Any,
                        &[
                            leaf("set", "Persist the default provider", RoleAccess::AdminOnly),
                            leaf(
                                "clear",
                                "Remove the default provider",
                                RoleAccess::AdminOnly,
                            ),
                        ],
                    ),
                ],
            ),
            group(
                "workflow",
                "Provision the Redmine workflow",
                RoleAccess::AdminOnly,
                ProviderScope::Any,
                &[leaf(
                    "bootstrap",
                    "Provision project and identities",
                    RoleAccess::AdminOnly,
                )],
            ),
        ],
    ),
    leaf("auth", "Moved to `admin auth setup`", RoleAccess::AdminOnly),
    group(
        "config",
        "Local configuration (writes under admin)",
        RoleAccess::Open,
        ProviderScope::Any,
        &[
            leaf("show", "Show the resolved configuration", RoleAccess::Open),
            group(
                "provider",
                "Machine-wide default provider",
                RoleAccess::Open,
                ProviderScope::Any,
                &[leaf("get", "Show the default provider", RoleAccess::Open)],
            ),
        ],
    ),
    group(
        "issue",
        "Issue operations",
        RoleAccess::Open,
        ProviderScope::Any,
        &[
            leaf("get", "Read one or more issues", ACCESS_ISSUE_READ),
            leaf("search", "Search issues", ACCESS_ISSUE_SEARCH),
            leaf("create", "Create an issue", ACCESS_ISSUE_CREATE),
            leaf(
                "update",
                "Update an issue body and planning",
                ACCESS_ISSUE_UPDATE,
            ),
            leaf("close", "Close an issue", ACCESS_ISSUE_CLOSE),
            leaf(
                "upload-attachment",
                "Upload an attachment",
                ACCESS_ATTACHMENT,
            ),
            leaf(
                "sync",
                "Reconcile leases against issue state",
                ACCESS_ORCHESTRATOR,
            ),
            leaf("bind", "Bind the branch to an issue", ACCESS_BIND),
            leaf("unbind", "Remove the branch binding", ACCESS_BIND),
            leaf("status", "Show the branch binding", RoleAccess::Open),
        ],
    ),
    group(
        "comment",
        "Comment operations",
        RoleAccess::Open,
        ProviderScope::Any,
        &[
            leaf(
                "create",
                "Create one authorized comment",
                ACCESS_COMMENT_CREATE,
            ),
            leaf("get", "Read one comment", ACCESS_COMMENT_READ),
            leaf(
                "list",
                "Read every comment on an issue",
                ACCESS_COMMENT_READ,
            ),
            leaf(
                "find-marker",
                "Find a comment by marker",
                ACCESS_FIND_MARKER,
            ),
        ],
    ),
    group(
        "project",
        "Redmine project operations",
        RoleAccess::Open,
        ProviderScope::Redmine,
        &[
            leaf("list", "List projects", ACCESS_PROJECT_READ),
            leaf("create", "Create a project", ACCESS_PROJECT_CREATE),
        ],
    ),
    group(
        "status",
        "Redmine issue status operations",
        RoleAccess::Open,
        ProviderScope::Redmine,
        &[
            leaf("list", "List issue statuses", ACCESS_STATUS_READ),
            leaf("next", "Show the policy next status", ACCESS_STATUS_READ),
            leaf("set", "Set an issue status", ACCESS_ORCHESTRATOR),
            leaf(
                "advance",
                "Advance with policy preflight",
                ACCESS_ORCHESTRATOR,
            ),
            leaf(
                "transition",
                "Alias of `status advance`",
                ACCESS_ORCHESTRATOR,
            ),
        ],
    ),
    group(
        "version",
        "Redmine project versions",
        RoleAccess::Open,
        ProviderScope::Redmine,
        &[leaf("list", "List project versions", ACCESS_VERSION_READ)],
    ),
    group(
        "relation",
        "Redmine issue relations",
        RoleAccess::Open,
        ProviderScope::Redmine,
        &[
            leaf("list", "List issue relations", ACCESS_RELATION_READ),
            leaf("create", "Create an issue relation", ACCESS_RELATION_CREATE),
            leaf("delete", "Delete an issue relation", ACCESS_RELATION_DELETE),
        ],
    ),
    group(
        "timer",
        "Redmine phase time tracking",
        RoleAccess::Open,
        ProviderScope::Redmine,
        &[
            leaf("start", "Open a timer run", ACCESS_ORCHESTRATOR),
            leaf("finish", "Close a timer run", ACCESS_ORCHESTRATOR),
            leaf("list", "List timer runs", ACCESS_ORCHESTRATOR),
            leaf("get", "Show one timer run", ACCESS_ORCHESTRATOR),
            leaf("recover", "Recover an orphan run", ACCESS_ORCHESTRATOR),
        ],
    ),
    leaf(
        "workflow",
        "Moved to `admin workflow bootstrap`",
        RoleAccess::AdminOnly,
    ),
    group(
        "worktree",
        "Local worktree leases",
        RoleAccess::Open,
        ProviderScope::Any,
        &[
            leaf("acquire", "Acquire a worktree lease", ACCESS_ORCHESTRATOR),
            leaf("release", "Release a worktree lease", ACCESS_ORCHESTRATOR),
            leaf("status", "Show a lease by issue", ACCESS_WORKTREE_READ),
            leaf("list", "List leases for a repository", ACCESS_WORKTREE_READ),
            leaf("probe", "Read-only lease diagnostic", ACCESS_WORKTREE_READ),
            leaf("prune", "Prune stale leases", ACCESS_ORCHESTRATOR),
            leaf(
                "heartbeat",
                "Refresh a lease heartbeat",
                ACCESS_ORCHESTRATOR,
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
        &[leaf(
            "send",
            "Deliver one bounded notification",
            ACCESS_NOTIFY,
        )],
    ),
    group(
        "mcp",
        "MCP server over stdio or HTTP",
        RoleAccess::Open,
        ProviderScope::Any,
        &[leaf(
            "serve",
            "Serve the contracted MCP tools",
            RoleAccess::AnyRole,
        )],
    ),
];
