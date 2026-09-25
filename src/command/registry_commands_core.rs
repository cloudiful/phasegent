//! Descriptor table (part 1): the workflow groups in declaration order, from
//! `gui` through `timer`/`workflow`. Spliced into `super::COMMANDS` by name, so
//! command order and every role/provider gate stay exactly as declared.

use super::super::{
    ACCESS_ATTACHMENT, ACCESS_BIND, ACCESS_COMMENT_CREATE, ACCESS_COMMENT_READ, ACCESS_FIND_MARKER,
    ACCESS_ISSUE_CLOSE, ACCESS_ISSUE_CREATE, ACCESS_ISSUE_READ, ACCESS_ISSUE_SEARCH,
    ACCESS_ISSUE_UPDATE, ACCESS_ORCHESTRATOR, ACCESS_PROJECT_CREATE, ACCESS_PROJECT_READ,
    ACCESS_RELATION_CREATE, ACCESS_RELATION_DELETE, ACCESS_RELATION_READ, ACCESS_STATUS_READ,
    ACCESS_VERSION_READ, CommandSpec, Feature, ProviderScope, RoleAccess, group, leaf, leaf_op,
};

pub(super) const CORE: &[CommandSpec] = &[
    CommandSpec {
        name: "gui",
        summary: "Open the desktop GUI (single-binary shell)",
        access: RoleAccess::Open,
        operation: "",
        feature: Some(Feature::Gui),
        provider_scope: ProviderScope::Any,
        children: &[],
    },
    leaf(
        "doctor",
        "Read-only self-check: credential presence, index backend, masked PG URL (no role needed)",
        RoleAccess::Open,
    ),
    group(
        "admin",
        "Human-operator provisioning: auth setup, config writes, workflow bootstrap (AI roles must never invoke)",
        RoleAccess::AdminOnly,
        ProviderScope::Any,
        &[
            group(
                "auth",
                "Store a provider credential",
                RoleAccess::AdminOnly,
                ProviderScope::Any,
                &[leaf_op(
                    "setup",
                    "Store the per-role credential",
                    // Credential setup stays role-scoped: every role provisions
                    // its own key (`PHASEGENT_ROLE=<role> ... admin auth setup`),
                    // while the rest of the admin group is human-only.
                    RoleAccess::AnyRole,
                    "admin auth setup",
                )],
            ),
            group(
                "config",
                "Persist settings and provider",
                RoleAccess::AdminOnly,
                ProviderScope::Any,
                &[
                    leaf_op(
                        "set",
                        "Persist a setting",
                        RoleAccess::AdminOnly,
                        "admin config set",
                    ),
                    leaf_op(
                        "clear",
                        "Remove a persisted setting",
                        RoleAccess::AdminOnly,
                        "admin config clear",
                    ),
                    group(
                        "provider",
                        "Machine-wide default provider",
                        RoleAccess::AdminOnly,
                        ProviderScope::Any,
                        &[
                            leaf_op(
                                "set",
                                "Persist the default provider",
                                RoleAccess::AdminOnly,
                                "admin config provider set",
                            ),
                            leaf_op(
                                "clear",
                                "Remove the default provider",
                                RoleAccess::AdminOnly,
                                "admin config provider clear",
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
                &[leaf_op(
                    "bootstrap",
                    "Provision project and identities",
                    RoleAccess::AdminOnly,
                    "admin workflow bootstrap",
                )],
            ),
        ],
    ),
    leaf_op(
        "auth",
        "Moved to `admin auth setup`",
        RoleAccess::AdminOnly,
        "auth",
    ),
    group(
        "config",
        "Local configuration (read-only show/get; writes live under admin)",
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
            leaf_op(
                "sync",
                "Reconcile leases against issue state",
                ACCESS_ORCHESTRATOR,
                "issue sync",
            ),
            leaf_op(
                "bind",
                "Bind the branch to an issue",
                ACCESS_BIND,
                "issue bind",
            ),
            leaf_op(
                "unbind",
                "Remove the branch binding",
                ACCESS_BIND,
                "issue unbind",
            ),
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
            leaf_op(
                "set",
                "Set an issue status",
                ACCESS_ORCHESTRATOR,
                "issue status update",
            ),
            leaf_op(
                "advance",
                "Advance with policy preflight",
                ACCESS_ORCHESTRATOR,
                "issue status update",
            ),
            leaf_op(
                "transition",
                "Alias of `status advance`",
                ACCESS_ORCHESTRATOR,
                "issue status update",
            ),
        ],
    ),
    group(
        "version",
        "Redmine project version operations",
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
        "Redmine phase time tracking (internal/auto; manual fallback only)",
        RoleAccess::Open,
        ProviderScope::Redmine,
        &[
            leaf_op(
                "start",
                "Open a timer run",
                ACCESS_ORCHESTRATOR,
                "timer start",
            ),
            leaf_op(
                "finish",
                "Close a timer run",
                ACCESS_ORCHESTRATOR,
                "timer finish",
            ),
            leaf_op("list", "List timer runs", ACCESS_ORCHESTRATOR, "timer list"),
            leaf_op(
                "get",
                "Show one timer run",
                ACCESS_ORCHESTRATOR,
                "timer get",
            ),
            leaf_op(
                "recover",
                "Recover an orphan run",
                ACCESS_ORCHESTRATOR,
                "timer recover",
            ),
        ],
    ),
    leaf_op(
        "workflow",
        "Moved to `admin workflow bootstrap`",
        RoleAccess::AdminOnly,
        "workflow",
    ),
];
