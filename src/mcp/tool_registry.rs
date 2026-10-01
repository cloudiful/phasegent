//! One descriptor table for the MCP tool surface (issue 597 phase 4).
//!
//! The `capabilities` tool lists this table filtered by role, `--help mcp`
//! renders the same list, and every handler resolves its role gate through
//! [`McpToolSpec::operation`] from one declared gate. A tool is named
//! `<group>_<action>`, so the exposed surface, its gates, and `Role::allows`
//! can never drift apart.
//!
//! [`ToolGate`] is the single place a tool's gate is declared, and it has one
//! arm per kind of surface:
//!
//! * [`ToolGate::Cli`] — the tool wraps a CLI command, so its gate is the
//!   shared registry's capability for that command path. This is the ordinary
//!   case and the reason MCP never invents a role list of its own.
//! * [`ToolGate::Explorer`] — the tool is a server-side delegation with no CLI
//!   command. Its role allowlist is the closed set in
//!   `crate::command::explorer`, referenced rather than restated, so the
//!   delegation roles have exactly one definition.
//! * [`ToolGate::Open`] — the open `capabilities` introspection tool, which
//!   every role that can start the MCP server may call.
//!
//! The table is the MCP allowlist: CLI-only surfaces (status writes, timers,
//! worktree lease mutations, admin, issue writes, comment reads, hooks, plugin
//! operations) are simply not entries here, so a handler cannot expose a
//! command the CLI would deny.

use crate::command::explorer::{DELEGATION_ROLES, EXPLORER_OPERATION};
use crate::command::{registry_allows_role, registry_capability};
use crate::policy::{Capability, Role};

/// Where one exposed tool's role gate comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolGate {
    /// A CLI registry path; the gate is that command's shared capability.
    Cli(&'static [&'static str]),
    /// The server-side explorer delegation, gated by
    /// [`DELEGATION_ROLES`].
    Explorer,
    /// No gate: open to every role that can start the MCP server.
    Open,
}

/// One exposed MCP tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct McpToolSpec {
    pub(crate) name: &'static str,
    pub(crate) gate: ToolGate,
}

impl McpToolSpec {
    pub(crate) const fn cli(name: &'static str, path: &'static [&'static str]) -> Self {
        Self {
            name,
            gate: ToolGate::Cli(path),
        }
    }

    pub(crate) const fn explorer(name: &'static str) -> Self {
        Self {
            name,
            gate: ToolGate::Explorer,
        }
    }

    pub(crate) const fn open(name: &'static str) -> Self {
        Self {
            name,
            gate: ToolGate::Open,
        }
    }

    /// Capability gate resolved from the shared CLI registry, for the tools
    /// that wrap a CLI command. `None` for the open and explorer surfaces,
    /// which are not provider commands.
    pub(crate) fn capability(self) -> Option<Capability> {
        match self.gate {
            ToolGate::Cli(path) => registry_capability(path),
            ToolGate::Explorer | ToolGate::Open => None,
        }
    }

    /// The stable permission `operation` a denial names. Every gate except the
    /// open introspection tool has one, so a handler gate and the protocol
    /// gate always report the same label.
    pub(crate) fn operation(self) -> &'static str {
        match self.gate {
            ToolGate::Cli(_) => self
                .capability()
                .map(Capability::operation)
                .unwrap_or("mcp tool"),
            ToolGate::Explorer => EXPLORER_OPERATION,
            ToolGate::Open => "",
        }
    }

    /// Whether `role` may call this tool.
    pub(crate) fn allows_role(self, role: Role) -> bool {
        match self.gate {
            ToolGate::Cli(path) => registry_allows_role(role, path),
            ToolGate::Explorer => DELEGATION_ROLES.contains(&role),
            ToolGate::Open => true,
        }
    }
}

pub(crate) const CAPABILITIES: McpToolSpec = McpToolSpec::open("capabilities");
pub(crate) const ISSUE_GET: McpToolSpec = McpToolSpec::cli("issue_get", &["issue", "get"]);
pub(crate) const ISSUE_SEARCH: McpToolSpec = McpToolSpec::cli("issue_search", &["issue", "search"]);
pub(crate) const STATUS_NEXT: McpToolSpec = McpToolSpec::cli("status_next", &["status", "next"]);
pub(crate) const COMMENT_CREATE: McpToolSpec =
    McpToolSpec::cli("comment_create", &["comment", "create"]);
pub(crate) const NOTIFY_SEND: McpToolSpec = McpToolSpec::cli("notify_send", &["notify", "send"]);

/// The five explorer operations, in the order the delegation contract declares
/// them. There is no run-list and no run deletion: a caller can start, read,
/// wait for, cancel, and resume a run it owns, and do nothing else.
pub(crate) const EXPLORER_START: McpToolSpec = McpToolSpec::explorer("explorer_start");
pub(crate) const EXPLORER_STATUS: McpToolSpec = McpToolSpec::explorer("explorer_status");
pub(crate) const EXPLORER_WAIT: McpToolSpec = McpToolSpec::explorer("explorer_wait");
pub(crate) const EXPLORER_CANCEL: McpToolSpec = McpToolSpec::explorer("explorer_cancel");
pub(crate) const EXPLORER_RESUME: McpToolSpec = McpToolSpec::explorer("explorer_resume");

/// The explorer group in one slice, for the group-level invariants the
/// explorer surface tests assert. The served tools are the individual consts
/// so each handler can name its own gate.
#[allow(dead_code)]
pub(crate) const EXPLORER_TOOLS: &[McpToolSpec] = &[
    EXPLORER_START,
    EXPLORER_STATUS,
    EXPLORER_WAIT,
    EXPLORER_CANCEL,
    EXPLORER_RESUME,
];

/// Every exposed tool, in declaration order. `capabilities` stays first so
/// introspection is always the leading entry in the advertised list, and the
/// explorer group follows the tracking tools so a role-filtered list keeps
/// reading as the tracking surface plus its delegation.
pub(crate) const TOOLS: &[McpToolSpec] = &[
    CAPABILITIES,
    ISSUE_GET,
    ISSUE_SEARCH,
    STATUS_NEXT,
    COMMENT_CREATE,
    NOTIFY_SEND,
    EXPLORER_START,
    EXPLORER_STATUS,
    EXPLORER_WAIT,
    EXPLORER_CANCEL,
    EXPLORER_RESUME,
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

    /// Every CLI-backed tool wraps a real registry path whose gate is a
    /// capability, so the descriptor cannot point at an unknown command or an
    /// ungated one.
    #[test]
    fn cli_tools_resolve_to_capability_gates() {
        for tool in TOOLS {
            let ToolGate::Cli(path) = tool.gate else {
                continue;
            };
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

    /// Per-role parity for the tracking surface: the descriptor-derived list is
    /// exactly what each role may call, and the notify row follows
    /// `Capability::Notify`.
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
                    "explorer_start",
                    "explorer_status",
                    "explorer_wait",
                    "explorer_cancel",
                    "explorer_resume",
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
                    "explorer_start",
                    "explorer_status",
                    "explorer_wait",
                    "explorer_cancel",
                    "explorer_resume",
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
                    "explorer_start",
                    "explorer_status",
                    "explorer_wait",
                    "explorer_cancel",
                    "explorer_resume",
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

    /// The explorer group is one gate applied to exactly five tools, and the
    /// role set is the closed delegation list rather than a second copy here.
    #[test]
    fn explorer_tools_share_one_declared_gate() {
        assert_eq!(EXPLORER_TOOLS.len(), 5);
        for tool in EXPLORER_TOOLS {
            assert_eq!(tool.gate, ToolGate::Explorer);
            assert!(tool.capability().is_none(), "{}", tool.name);
            assert_eq!(tool.operation(), EXPLORER_OPERATION);
            for role in ALL_ROLES {
                assert_eq!(
                    tool.allows_role(*role),
                    DELEGATION_ROLES.contains(role),
                    "{} gate for {role}",
                    tool.name
                );
            }
        }
    }

    /// Every gated tool names a permission operation, so a denial from either
    /// gate reports the same label.
    #[test]
    fn every_gated_tool_names_an_operation() {
        for tool in TOOLS {
            if tool.gate == ToolGate::Open {
                assert_eq!(tool.operation(), "", "{}", tool.name);
                continue;
            }
            assert!(!tool.operation().is_empty(), "{}", tool.name);
        }
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
                    tool.gate,
                    ToolGate::Cli(excluded),
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
