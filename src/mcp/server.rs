//! MCP transports: stdio (default) and streamable HTTP via axum.
//!
//! The sync CLI stays sync. Each entry builds a scoped Tokio runtime
//! and blocks once; async work never escapes the transport. Stdio
//! keeps stdout as the JSON-RPC channel (diagnostics go to stderr);
//! HTTP mounts the rmcp tower service at `/mcp` with graceful
//! shutdown.

use crate::mcp::tools::{McpConfig, PhasegentMcpServer};

/// Environment variable carrying the HTTP bearer token. Read only
/// for `--transport http`; never a CLI flag and never logged.
pub(crate) const MCP_HTTP_AUTH_TOKEN_ENV: &str = "PHASEGENT_MCP_AUTH_TOKEN";

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
/// gracefully on Ctrl-C. Fails closed when
/// `PHASEGENT_MCP_AUTH_TOKEN` is missing or empty; the token value
/// itself is never logged.
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
    let expected_token = match http_auth_token_from_env() {
        Ok(token) => token,
        Err(message) => {
            eprintln!(
                "{}",
                serde_json::json!({"error":{"kind":"argument","message":message}})
            );
            return 2;
        }
    };
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
    match runtime.block_on(serve_http_async(config, bind.to_owned(), expected_token)) {
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

/// Read the HTTP bearer token from the environment. Fails closed on
/// missing, empty, or whitespace-only values. The returned token is
/// the trimmed value; callers must never log it.
pub(crate) fn http_auth_token_from_env() -> Result<String, String> {
    let raw = std::env::var(MCP_HTTP_AUTH_TOKEN_ENV).ok();
    normalize_auth_token(raw.as_deref())
}

/// Pure helper for [`http_auth_token_from_env`] so unit tests stay
/// hermetic without touching the process environment.
pub(crate) fn normalize_auth_token(raw: Option<&str>) -> Result<String, String> {
    match raw {
        Some(value) if !value.trim().is_empty() => Ok(value.trim().to_owned()),
        _ => Err(format!(
            "mcp serve --transport http requires {MCP_HTTP_AUTH_TOKEN_ENV} to be set (non-empty)"
        )),
    }
}

/// Exact `Authorization: Bearer <expected>` check. No prefix
/// fallback, no trimming of the header value, and no token leakage
/// in the boolean result.
pub(crate) fn is_bearer_authorized(header: Option<&str>, expected: &str) -> bool {
    match header {
        Some(value) => value == format!("Bearer {expected}"),
        None => false,
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

/// Build the authenticated HTTP router. Every `/mcp` request goes
/// through the bearer middleware; the 401 body never contains the
/// token. Extracted so the auth layer is unit-testable without
/// binding a port.
pub(crate) fn build_http_router(config: McpConfig, expected_token: String) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    let service = StreamableHttpService::new(
        move || Ok(PhasegentMcpServer::new(config.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let auth = axum::middleware::from_fn_with_state(expected_token, require_bearer_auth);
    axum::Router::new()
        .nest_service("/mcp", service)
        .layer(auth)
}

async fn require_bearer_auth(
    axum::extract::State(expected): axum::extract::State<String>,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let authorized = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| is_bearer_authorized(Some(value), &expected));
    if authorized {
        next.run(req).await
    } else {
        unauthorized_response()
    }
}

fn unauthorized_response() -> axum::response::Response {
    use axum::{Json, http::StatusCode, response::IntoResponse};

    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error":{"kind":"unauthorized","message":"missing or invalid Authorization Bearer token"}})),
    )
        .into_response()
}

async fn serve_http_async(
    config: McpConfig,
    bind: String,
    expected_token: String,
) -> Result<(), String> {
    let router = build_http_router(config, expected_token);
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

    #[test]
    fn auth_token_rejects_missing_empty_and_whitespace() {
        assert!(normalize_auth_token(None).is_err());
        assert!(normalize_auth_token(Some("")).is_err());
        assert!(normalize_auth_token(Some("   ")).is_err());
        assert!(normalize_auth_token(Some("\n\t ")).is_err());
        let error = normalize_auth_token(None).unwrap_err();
        assert!(
            error.contains(MCP_HTTP_AUTH_TOKEN_ENV),
            "error must name the env var; got {error}"
        );
        assert!(
            !error.contains("secret"),
            "error must not echo any token; got {error}"
        );
    }

    #[test]
    fn auth_token_accepts_and_trims_value() {
        assert_eq!(
            normalize_auth_token(Some("s3cret")).unwrap(),
            "s3cret".to_owned()
        );
        // Surrounding whitespace is accidental shell formatting, not
        // part of the secret.
        assert_eq!(
            normalize_auth_token(Some("  s3cret  ")).unwrap(),
            "s3cret".to_owned()
        );
    }

    #[test]
    fn bearer_check_requires_exact_match() {
        assert!(is_bearer_authorized(Some("Bearer s3cret"), "s3cret"));
        assert!(!is_bearer_authorized(None, "s3cret"));
        assert!(!is_bearer_authorized(Some(""), "s3cret"));
        assert!(!is_bearer_authorized(Some("Bearer wrong"), "s3cret"));
        assert!(!is_bearer_authorized(Some("bearer s3cret"), "s3cret"));
        assert!(!is_bearer_authorized(Some("Bearer  s3cret"), "s3cret"));
        assert!(!is_bearer_authorized(Some("Bearer s3cret "), "s3cret"));
        assert!(!is_bearer_authorized(Some("s3cret"), "s3cret"));
        assert!(!is_bearer_authorized(Some("Bearer "), "s3cret"));
    }
}
