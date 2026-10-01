//! The composed tool router and the one role gate every MCP entry point uses.
//!
//! The server serves two tool groups — the tracking tools declared in
//! [`tools`](super::tools) and the explorer delegation in
//! [`explorer_tools`](super::explorer_tools) — so the router is composed here
//! rather than in either handler module. `ToolRouter` is additive, which keeps
//! the composition to one expression and keeps both handler modules free of
//! knowledge about each other.
//!
//! The role gate lives here too, and in one place: [`PhasegentMcpServer::gate`]
//! is called by the protocol-level `tools/call` before the router deserializes
//! anything, and by each handler's own `require_tool` as defense in depth. Both
//! resolve the same [`McpToolSpec`], so a tool can never be advertised, listed,
//! or dispatched to a role its declared gate denies.

use rmcp::{
    ErrorData as McpError, RoleServer,
    handler::server::router::tool::ToolRouter,
    model::{
        CacheScope, CallToolRequestParams, ListToolsResult, ProtocolVersion, ResultType, Tool,
    },
    service::RequestContext,
};

use crate::policy::Role;

use super::tool_registry::{self, McpToolSpec};
use super::tools::PhasegentMcpServer;

impl PhasegentMcpServer {
    /// Every served tool: the tracking router plus the explorer router.
    pub(crate) fn tool_router() -> ToolRouter<Self> {
        Self::tracking_tool_router() + Self::explorer_tool_router()
    }

    /// The role gate for one declared tool. `None` for the open
    /// introspection tool, which every role may call.
    ///
    /// The error carries the tool's declared operation, so a denial names the
    /// same operation the execution layer would.
    pub(crate) fn gate(&self, tool: McpToolSpec) -> Result<(), McpError> {
        if tool.allows_role(self.role()) {
            return Ok(());
        }
        Err(super::tools::permission_error(
            self.role(),
            tool.operation(),
        ))
    }

    pub(crate) fn role(&self) -> Role {
        self.config.role
    }

    /// The protocol-level `tools/list` filter: the router descriptors kept to
    /// the startup role's allowlist, so a client sees the same set as the
    /// `capabilities` payload and role-specific `--help mcp`.
    pub(crate) fn advertised_tools(&self) -> Vec<Tool> {
        Self::tool_router()
            .list_all()
            .into_iter()
            .filter(|tool| self.tool_allowed(tool.name.as_ref()))
            .collect()
    }

    /// Whether the startup role may see and call the named tool. Unknown names
    /// stay `false`, so the protocol surface filters them out while
    /// `tools/call` keeps the router's not-found contract for a name no
    /// handler is registered for.
    pub(crate) fn tool_allowed(&self, name: &str) -> bool {
        tool_registry::TOOLS
            .iter()
            .any(|tool| tool.name == name && tool.allows_role(self.role()))
    }

    /// The denial for an unadvertised tool, resolved before the router sees
    /// the request. Returns `None` when the tool is advertised for the startup
    /// role, so an allowed call proceeds and a name with no route keeps the
    /// router's not-found error.
    pub(crate) fn denial_for(&self, request: &CallToolRequestParams) -> Option<McpError> {
        tool_registry::TOOLS
            .iter()
            .find(|tool| tool.name == request.name.as_ref())
            .filter(|tool| !tool.allows_role(self.role()))
            .map(|tool| super::tools::permission_error(self.role(), tool.operation()))
    }

    /// The `tools/list` result contract (result type, empty pagination,
    /// protocol cache hints) with the role filter applied. The server's
    /// allowlist is complete and bounded, so a client-supplied cursor never
    /// applies and the result type stays complete.
    pub(crate) fn list_tools_result(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> ListToolsResult {
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28);
        ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: self.advertised_tools(),
            meta: None,
            next_cursor: None,
            ttl_ms: supports_cache_hints.then_some(0),
            cache_scope: supports_cache_hints.then_some(CacheScope::Public),
        }
    }
}
