//! rmcp MCP server surface for phasegent.
//!
//! Tools wrap the existing provider and notification paths without
//! reimplementing them. The server never trusts client-supplied roles:
//! every tool runs with the [`McpConfig`] captured at `mcp serve`
//! startup. Transports are stdio (default) and streamable HTTP via
//! axum on `/mcp`; the sync CLI stays sync through scoped runtimes.

pub mod server;
pub mod tools;
