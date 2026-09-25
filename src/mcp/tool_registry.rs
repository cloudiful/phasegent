//! One descriptor table for the MCP tool surface (issue 597 phase 4).
//!
//! The `capabilities` tool lists this table filtered by role, `--help mcp`
//! renders the same list, and every handler resolves its role gate through
//! [`McpToolSpec::capability`] from the shared CLI command registry. A tool is
//! named `<group>_<action>` and maps to the CLI registry path
//! `["<group>", "<action>"]`, so the exposed surface, its gates, and
//! `Role::allows` can never drift apart. Only the open `capabilities`
//! introspection tool has no CLI path.
//!
//! The table is the MCP allowlist: CLI-only surfaces (status writes, timers,
//! worktree lease mutations, admin, issue writes, comment reads, hooks, plugin
//! operations) are simply not entries here, so a handler cannot expose a
//! command the CLI would deny.

use crate::command::{registry_allows_role, registry_capability};
use crate::policy::{Capability, Role};

/// One exposed MCP tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct McpToolSpec {
    pub(crate) name: &'static str,
    /// CLI registry path backing the tool. `None` only for the open
    /// `capabilities` introspection tool, which no CLI command backs.
    pub(crate) cli_path: Option<&'static [&'static str]>,
}

impl McpToolSpec {
    /// Capability gate resolved from the shared CLI registry. `None` means the
    /// tool is open to every role that can start the MCP server.
    pub(crate) fn capability(self) -> Option<Capability> {
        match self.cli_path {
            Some(path) => registry_capability(path),
            None => None,
        }
    }

    /// Whether `role` may call this tool, resolved through the shared registry
    /// gate so MCP never invents its own role list.
    pub(crate) fn allows_role(self, role: Role) -> bool {
        match self.cli_path {
            Some(path) => registry_allows_role(role, path),
            None => true,
        }
    }
}

pub(crate) const CAPABILITIES: McpToolSpec = McpToolSpec {
    name: "capabilities",
    cli_path: None,
};
pub(crate) const ISSUE_GET: McpToolSpec = McpToolSpec {
    name: "issue_get",
    cli_path: Some(&["issue", "get"]),
};
pub(crate) const ISSUE_SEARCH: McpToolSpec = McpToolSpec {
    name: "issue_search",
    cli_path: Some(&["issue", "search"]),
};
pub(crate) const STATUS_NEXT: McpToolSpec = McpToolSpec {
    name: "status_next",
    cli_path: Some(&["status", "next"]),
};
pub(crate) const COMMENT_CREATE: McpToolSpec = McpToolSpec {
    name: "comment_create",
    cli_path: Some(&["comment", "create"]),
};
pub(crate) const NOTIFY_SEND: McpToolSpec = McpToolSpec {
    name: "notify_send",
    cli_path: Some(&["notify", "send"]),
};

/// Every exposed tool, in declaration order. `capabilities` stays first so
/// introspection is always the leading entry in the advertised list.
pub(crate) const TOOLS: &[McpToolSpec] = &[
    CAPABILITIES,
    ISSUE_GET,
    ISSUE_SEARCH,
    STATUS_NEXT,
    COMMENT_CREATE,
    NOTIFY_SEND,
];

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ROLES: &[Role] = &[
        Role::Admin,
        Role::Orchestrator,
        Role::Executor,
        Role::Reviewer,
        Role::Tester,
    ];

    fn names(role: Role) -> Vec<&'static str> {
        TOOLS
            .iter()
            .filter(|tool| tool.allows_role(role))
            .map(|tool| tool.name)
            .collect()
    }

    #[test]
    fn every_tool_name_is_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for tool in TOOLS {
            assert!(
                !seen.contains(&tool.name),
                "duplicate MCP tool '{}'",
                tool.name
            );
            seen.push(tool.name);
        }
    }

    /// Every non-open tool wraps a real CLI registry path whose gate is a
    /// capability, so the descriptor cannot point at an unknown command or an
    /// ungated one.
    #[test]
    fn tool_paths_resolve_to_capability_gates() {
        for tool in TOOLS {
            match tool.cli_path {
                None => {
                    assert_eq!(tool.name, "capabilities");
                    assert_eq!(tool.capability(), None);
                }
                Some(path) => {
                    let capability = tool
                        .capability()
                        .unwrap_or_else(|| panic!("{} must resolve a capability", tool.name));
                    assert_eq!(
                        path.join("_"),
                        tool.name,
                        "tool names must mirror their CLI path"
                    );
                    for role in ALL_ROLES {
                        assert_eq!(
                            tool.allows_role(*role),
                            role.allows(capability),
                            "{} gate drifted from Role::allows for {role}",
                            tool.name
                        );
                    }
                }
            }
        }
    }

    /// Per-role parity: the descriptor-derived list is exactly what each role
    /// may call, and the notify row follows `Capability::Notify`.
    #[test]
    fn role_tool_lists_match_the_policy() {
        let expected: &[(Role, &[&str])] = &[
            (
                Role::Orchestrator,
                &[
                    "capabilities",
                    "issue_get",
                    "issue_search",
                    "status_next",
                    "comment_create",
                    "notify_send",
                ],
            ),
            (
                Role::Executor,
                &[
                    "capabilities",
                    "issue_get",
                    "status_next",
                    "comment_create",
                    "notify_send",
                ],
            ),
            (
                Role::Reviewer,
                &[
                    "capabilities",
                    "issue_get",
                    "status_next",
                    "comment_create",
                    "notify_send",
                ],
            ),
            (
                Role::Tester,
                &["capabilities", "issue_get", "comment_create", "notify_send"],
            ),
            (Role::Admin, &["capabilities", "status_next"]),
        ];
        for (role, expected) in expected {
            assert_eq!(&names(*role), expected, "tool list for {role}");
        }
    }

    /// The open introspection tool stays available to every role that can start
    /// the MCP server, and `notify_send` is denied only to admin.
    #[test]
    fn capabilities_stays_open_and_notify_follows_the_capability() {
        assert!(CAPABILITIES.allows_role(Role::Admin));
        for role in ALL_ROLES {
            assert!(
                names(*role).first() == Some(&"capabilities"),
                "capabilities must lead the list for {role}"
            );
            assert_eq!(
                NOTIFY_SEND.allows_role(*role),
                role.allows(Capability::Notify),
                "notify_send for {role}"
            );
        }
        assert!(NOTIFY_SEND.allows_role(Role::Orchestrator));
        assert!(!NOTIFY_SEND.allows_role(Role::Admin));
    }

    /// CLI-only operations must never be reachable as an MCP tool.
    #[test]
    fn excluded_cli_operations_are_not_mcp_tools() {
        let excluded_paths: &[&[&str]] = &[
            &["status", "set"],
            &["status", "advance"],
            &["status", "transition"],
            &["timer", "start"],
            &["timer", "finish"],
            &["worktree", "acquire"],
            &["worktree", "release"],
            &["worktree", "prune"],
            &["worktree", "heartbeat"],
            &["admin"],
            &["admin", "config"],
            &["issue", "create"],
            &["issue", "update"],
            &["issue", "close"],
            &["issue", "upload-attachment"],
            &["comment", "get"],
            &["comment", "list"],
            &["comment", "find-marker"],
            &["hooks", "install"],
            &["hooks", "run"],
            &["plugin", "install"],
            &["plugin", "status"],
            &["plugin", "uninstall"],
        ];
        for excluded in excluded_paths {
            for tool in TOOLS {
                assert_ne!(
                    tool.cli_path,
                    Some(*excluded),
                    "MCP must not expose CLI path {excluded:?}"
                );
            }
        }
        for tool in TOOLS {
            for needle in [
                "status_advance",
                "timer",
                "worktree",
                "admin",
                "hooks",
                "plugin",
            ] {
                assert!(
                    !tool.name.contains(needle),
                    "MCP tool {} must not expose {needle}",
                    tool.name
                );
            }
        }
    }
}
