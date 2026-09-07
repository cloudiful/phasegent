//! MCP transports: stdio (default) and streamable HTTP via axum.
//!
//! The sync CLI stays sync. Each entry builds a scoped Tokio runtime
//! and blocks once; async work never escapes the transport. Stdio
//! keeps stdout as the JSON-RPC channel (diagnostics go to stderr);
//! HTTP mounts the rmcp tower service at `/mcp` with graceful
//! shutdown.

use crate::mcp::tools::{McpConfig, PhasegentMcpServer};

/// CLI entry for `mcp serve`. Builds the server-side [`McpConfig`]
/// from invocation flags and dispatches to the scoped transport.
/// Runs synchronously; async work stays inside the transport.
pub fn execute(
    role: crate::policy::Role,
    provider: Option<crate::providers::ProviderKind>,
    api_base: Option<String>,
    repository: Option<String>,
    project_id: Option<String>,
    close_status_id: Option<String>,
    command: crate::command::McpCommand,
) -> i32 {
    let crate::command::McpCommand::Serve {
        transport,
        bind,
        authorized,
    } = command;
    let config = McpConfig::new(
        role,
        provider,
        api_base,
        repository,
        project_id,
        close_status_id,
        authorized,
    );
    match transport {
        crate::command::McpTransport::Stdio => serve_stdio(config),
        crate::command::McpTransport::Http => serve_http(config, &bind),
    }
}

/// Serve MCP over stdio. Sync wrapper for the CLI; returns a
/// process exit code and never writes non-protocol bytes to stdout.
pub fn serve_stdio(config: McpConfig) -> i32 {
    if tokio::runtime::Handle::try_current().is_ok() {
        eprintln!(
            "{}",
            serde_json::json!({"error":{"kind":"runtime","message":"mcp serve must not run inside a Tokio runtime"}})
        );
        return 1;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"error":{"kind":"runtime","message":format!("could not build mcp runtime: {error}")}})
            );
            return 1;
        }
    };
    match runtime.block_on(serve_stdio_async(config)) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!(
                "{}",
                serde_json::json!({"error":{"kind":"mcp","operation":"mcp serve","message":message}})
            );
            1
        }
    }
}

/// Serve MCP over streamable HTTP on `bind` (e.g.
/// `127.0.0.1:3000`) with `/mcp` mounted. Sync wrapper; shuts down
/// gracefully on Ctrl-C.
pub fn serve_http(config: McpConfig, bind: &str) -> i32 {
    if tokio::runtime::Handle::try_current().is_ok() {
        eprintln!(
            "{}",
            serde_json::json!({"error":{"kind":"runtime","message":"mcp serve must not run inside a Tokio runtime"}})
        );
        return 1;
    }
    if let Err(message) = validate_bind(bind) {
        eprintln!(
            "{}",
            serde_json::json!({"error":{"kind":"argument","message":message}})
        );
        return 2;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"error":{"kind":"runtime","message":format!("could not build mcp runtime: {error}")}})
            );
            return 1;
        }
    };
    match runtime.block_on(serve_http_async(config, bind.to_owned())) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!(
                "{}",
                serde_json::json!({"error":{"kind":"mcp","operation":"mcp serve","message":message}})
            );
            1
        }
    }
}

pub(crate) fn validate_bind(bind: &str) -> Result<(), String> {
    let trimmed = bind.trim();
    if trimmed.is_empty() {
        return Err("mcp serve --bind cannot be empty".to_owned());
    }
    trimmed
        .parse::<std::net::SocketAddr>()
        .map(|_| ())
        .map_err(|_| "mcp serve --bind must be a socket address like 127.0.0.1:3000".to_owned())
}

async fn serve_stdio_async(config: McpConfig) -> Result<(), String> {
    use rmcp::{ServiceExt, transport::stdio};

    let service = PhasegentMcpServer::new(config)
        .serve(stdio())
        .await
        .map_err(|error| format!("mcp stdio serve failed: {error}"))?;
    service
        .waiting()
        .await
        .map(|_| ())
        .map_err(|error| format!("mcp stdio terminated: {error}"))
}

async fn serve_http_async(config: McpConfig, bind: String) -> Result<(), String> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    let service = StreamableHttpService::new(
        move || Ok(PhasegentMcpServer::new(config.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|error| format!("could not bind {bind}: {error}"))?;
    eprintln!(
        "{}",
        serde_json::json!({"mcp":{"transport":"http","bind":bind,"endpoint":"/mcp"}})
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|error| format!("mcp http terminated: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_validation_accepts_loopback_and_rejects_garbage() {
        assert!(validate_bind("127.0.0.1:3000").is_ok());
        assert!(validate_bind("").is_err());
        assert!(validate_bind("not-an-addr").is_err());
    }
}
