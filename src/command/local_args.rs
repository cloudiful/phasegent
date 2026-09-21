#[derive(Debug)]
pub enum PluginCommand {
    Install {
        global: bool,
        project: bool,
        force: bool,
    },
    Status,
    Uninstall {
        global: bool,
        project: bool,
    },
}

/// Agent notification send. `event` is the structured kind
/// (completion, blocked, failure, interruption_suspected,
/// publish_failed); `title`/`body` are bounded at the envelope layer
/// and persisted before delivery.
#[derive(Debug)]
pub enum NotifyCommand {
    Send {
        event: crate::notifications::NotificationEvent,
        title: String,
        body: String,
        issue: Option<u64>,
        phase: Option<String>,
    },
}

/// MCP transport selector for `mcp serve`. Stdio is the default
/// local transport; HTTP serves streamable HTTP via axum on `/mcp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransport {
    Stdio,
    Http,
}

impl McpTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }
}

impl std::fmt::Display for McpTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `mcp serve` invocation. `bind` is the HTTP socket address (used
/// only for `--transport http`); `authorized` is the server-side
/// opt-in that enables the `comment_create` tool for non-orchestrator
/// roles. No client-supplied role is ever trusted.
#[derive(Debug)]
pub enum McpCommand {
    Serve {
        transport: McpTransport,
        bind: String,
        authorized: bool,
    },
}
