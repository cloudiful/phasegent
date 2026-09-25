//! MCP tools bound to the server-side role.
//!
//! Every tool runs with the [`McpConfig`] captured at `mcp serve`
//! startup. Client requests never supply a role, provider, or
//! credential. Tools call the same provider and notification paths
//! as the CLI; they do not reimplement HTTP or delivery.

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{tool::ToolCallContext, wrapper::Parameters},
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
        InitializeRequestParams, InitializeResult, ListToolsResult, PaginatedRequestParams,
        ProtocolVersion, ResultType, Tool,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};

use crate::policy::{Capability, Role};
use crate::providers::{IssueProvider, ProviderDispatcher, ProviderKind};

use super::tool_registry::{self, McpToolSpec};

/// Server-side invocation context. Captured once at `mcp serve`
/// startup from `PHASEGENT_ROLE` and the provider flags; never taken
/// from MCP client input.
#[derive(Clone, Debug)]
pub struct McpConfig {
    pub role: Role,
    pub provider: Option<ProviderKind>,
    pub api_base: Option<String>,
    pub repository: Option<String>,
    pub project_id: Option<String>,
    pub close_status_id: Option<String>,
    /// Explicit server-side opt-in for `comment_create`. Mirrors the
    /// CLI `--authorized` gate but lives on the server command line
    /// so MCP clients cannot self-authorize.
    pub authorized: bool,
}

impl McpConfig {
    pub fn new(
        role: Role,
        provider: Option<ProviderKind>,
        api_base: Option<String>,
        repository: Option<String>,
        project_id: Option<String>,
        close_status_id: Option<String>,
        authorized: bool,
    ) -> Self {
        Self {
            role,
            provider,
            api_base,
            repository,
            project_id,
            close_status_id,
            authorized,
        }
    }
}

/// rmcp server exposing the contracted tool set.
///
/// Contracted tools: `capabilities`, `issue_get`, `issue_search`,
/// `status_next`, `comment_create` (server-side `--authorized`
/// only), `notify_send`. Explicitly excluded: `status_advance`,
/// timer start/finish, and any role elevation.
#[derive(Clone)]
pub struct PhasegentMcpServer {
    config: McpConfig,
}

impl PhasegentMcpServer {
    pub fn new(config: McpConfig) -> Self {
        Self { config }
    }

    fn dispatcher(&self) -> Result<ProviderDispatcher, McpError> {
        crate::cli::provider_for(
            self.config.role,
            self.config.provider,
            self.config.api_base.as_deref(),
            self.config.repository.as_deref(),
            self.config.project_id.as_deref(),
            self.config.close_status_id.as_deref(),
        )
        .map_err(|error| internal_error(&error.to_string()))
    }

    /// Role gate for one registered tool. The capability comes from the shared
    /// CLI registry via [`McpToolSpec::capability`], so every handler and the
    /// advertised `capabilities` list share one descriptor table instead of a
    /// per-handler role list.
    fn require_tool(&self, tool: McpToolSpec) -> Result<(), McpError> {
        match tool.capability() {
            Some(capability) if !self.config.role.allows(capability) => {
                Err(permission_error(self.config.role, capability.operation()))
            }
            _ => Ok(()),
        }
    }

    /// Whether the startup role may see and call the named tool. Unknown names
    /// stay `false`, so the protocol surface filters them out while
    /// `tools/call` keeps the router's not-found contract for a name no
    /// handler is registered for.
    fn tool_allowed_for_role(&self, name: &str) -> bool {
        tool_registry::TOOLS
            .iter()
            .any(|tool| tool.name == name && tool.allows_role(self.config.role))
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct IssueGetParams {
    /// Issue number (positive integer).
    number: u64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct IssueSearchParams {
    /// Free-text query; required unless `all` is true.
    query: Option<String>,
    /// open, closed, or all (default open).
    state: Option<String>,
    /// 1-based page (default 1).
    page: Option<usize>,
    /// Page size 1-100 (default 50).
    limit: Option<usize>,
    /// Bounded all-issues listing when no query is given.
    all: Option<bool>,
    /// Include truncated bodies.
    include_body: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StatusNextParams {
    /// Issue number (positive integer).
    number: u64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CommentCreateParams {
    /// Target issue number.
    issue: u64,
    /// Comment body; must contain `marker`.
    body: String,
    /// Marker embedded in `body` for idempotent lookup.
    marker: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct NotifySendParams {
    /// completion, blocked, failure, interruption_suspected, publish_failed.
    event: String,
    /// Short summary (truncated to 140 chars; rejected above 2000 chars).
    title: String,
    /// Bounded detail (optional, truncated to 2000 chars).
    body: Option<String>,
    /// Optional positive issue id.
    issue: Option<u64>,
    /// Optional phase label stored as metadata.
    phase: Option<String>,
}

#[tool_router]
impl PhasegentMcpServer {
    #[tool(
        name = "capabilities",
        description = "List MCP tools allowed for the server role and provider. No status_advance, timer, or role elevation is exposed."
    )]
    fn capabilities(&self) -> Result<CallToolResult, McpError> {
        let allowed = allowed_tools(self.config.role);
        let payload = serde_json::json!({
            "role": self.config.role.as_str(),
            "provider": self.config.provider.map(|kind| kind.as_str()),
            "tools": allowed,
            "comment_create_authorized": self.config.authorized,
        });
        ok_json(&payload)
    }

    #[tool(name = "issue_get", description = "Read one issue by number.")]
    fn issue_get(
        &self,
        Parameters(params): Parameters<IssueGetParams>,
    ) -> Result<CallToolResult, McpError> {
        self.require_tool(tool_registry::ISSUE_GET)?;
        if params.number == 0 {
            return Err(invalid_params("issue_get number must be greater than zero"));
        }
        let provider = self.dispatcher()?;
        if !provider.supports(Capability::IssueRead) {
            return Err(not_supported(
                provider.kind().as_str(),
                Capability::IssueRead.operation(),
            ));
        }
        provider
            .get_issue(params.number)
            .map(|summary| ok_json_value(&serde_json::to_value(&summary).unwrap_or_default()))
            .map_err(|error| internal_error(&error.to_string()))?
    }

    #[tool(
        name = "issue_search",
        description = "Search issues. Requires query or all=true. Provider-direct; no stale local fallback."
    )]
    fn issue_search(
        &self,
        Parameters(params): Parameters<IssueSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        self.require_tool(tool_registry::ISSUE_SEARCH)?;
        let options = crate::providers::IssueSearchOptions {
            query: params.query.clone(),
            state: params.state.clone().unwrap_or_else(|| "open".to_owned()),
            page: params.page.unwrap_or(1),
            limit: params.limit.unwrap_or(50),
            include_body: params.include_body.unwrap_or(false),
            all: params.all.unwrap_or(false),
        };
        options
            .validate()
            .map_err(|error| invalid_params(&error.to_string()))?;
        let provider = self.dispatcher()?;
        if !provider.supports(Capability::IssueSearch) {
            return Err(not_supported(
                provider.kind().as_str(),
                Capability::IssueSearch.operation(),
            ));
        }
        provider
            .search_issue_page(&options)
            .map(|page| {
                let output = crate::providers::IssueSearchResult {
                    items: page
                        .items
                        .into_iter()
                        .map(|summary| {
                            crate::providers::api::IssueSearchItem::from_summary(
                                summary,
                                options.include_body,
                            )
                        })
                        .collect(),
                    page: page.page,
                    limit: page.limit,
                    total_count: page.total_count,
                    has_more: page.has_more,
                };
                ok_json_value(&serde_json::to_value(&output).unwrap_or_default())
            })
            .map_err(|error| internal_error(&error.to_string()))?
    }

    #[tool(
        name = "status_next",
        description = "Read current status plus policy-allowed next statuses. Redmine-only."
    )]
    fn status_next(
        &self,
        Parameters(params): Parameters<StatusNextParams>,
    ) -> Result<CallToolResult, McpError> {
        self.require_tool(tool_registry::STATUS_NEXT)?;
        if params.number == 0 {
            return Err(invalid_params(
                "status_next number must be greater than zero",
            ));
        }
        let provider = self.dispatcher()?;
        match provider {
            ProviderDispatcher::Redmine(redmine) => redmine
                .status_next(params.number)
                .map(|output| ok_json_value(&serde_json::to_value(&output).unwrap_or_default()))
                .map_err(|error| internal_error(&error.to_string()))?,
            other => Err(not_supported(other.kind().as_str(), "issue status next")),
        }
    }

    #[tool(
        name = "comment_create",
        description = "Create one comment. Requires server-side --authorized unless the server role is orchestrator. Body must contain marker."
    )]
    fn comment_create(
        &self,
        Parameters(params): Parameters<CommentCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.require_tool(tool_registry::COMMENT_CREATE)?;
        if self.config.role != Role::Orchestrator && !self.config.authorized {
            return Err(McpError::internal_error(
                "comment create requires server-side --authorized for this role".to_owned(),
                None,
            ));
        }
        if params.issue == 0 {
            return Err(invalid_params(
                "comment_create issue must be greater than zero",
            ));
        }
        if params.marker.trim().is_empty() {
            return Err(invalid_params("comment_create marker cannot be empty"));
        }
        if !params.body.contains(&params.marker) {
            return Err(invalid_params("comment_create body must contain marker"));
        }
        let provider = self.dispatcher()?;
        if !provider.supports(Capability::CommentCreate) {
            return Err(not_supported(
                provider.kind().as_str(),
                Capability::CommentCreate.operation(),
            ));
        }
        provider
            .create_comment(params.issue, &params.body, &params.marker)
            .map(|output| ok_json_value(&serde_json::to_value(&output).unwrap_or_default()))
            .map_err(|error| internal_error(&error.to_string()))?
    }

    #[tool(
        name = "notify_send",
        description = "Deliver one bounded notification envelope on the configured channel. Persisted before delivery."
    )]
    fn notify_send(
        &self,
        Parameters(params): Parameters<NotifySendParams>,
    ) -> Result<CallToolResult, McpError> {
        self.require_tool(tool_registry::NOTIFY_SEND)?;
        let event = crate::notifications::NotificationEvent::parse(&params.event)
            .map_err(|message| invalid_params(&message))?;
        if params.title.trim().is_empty() {
            return Err(invalid_params("notify_send title cannot be empty"));
        }
        if params.title.chars().count() > 2000 {
            return Err(invalid_params("notify_send title is too long"));
        }
        if params.body.as_deref().unwrap_or("").chars().count() > 10000 {
            return Err(invalid_params("notify_send body is too long"));
        }
        if let Some(issue) = params.issue
            && issue == 0
        {
            return Err(invalid_params(
                "notify_send issue must be greater than zero",
            ));
        }
        if let Some(phase) = params.phase.as_deref() {
            reject_notify_setting_phase(phase)?;
        }
        let mut intent = crate::notifications::NotificationIntent::new(
            event,
            params.title.clone(),
            params.body.clone().unwrap_or_default(),
        );
        if let Some(issue) = params.issue {
            intent = intent.with_issue(issue);
        }
        if let Some(phase) = params.phase.clone() {
            let trimmed = phase.trim().to_owned();
            if !trimmed.is_empty() {
                intent = intent.with_meta("phase", trimmed);
            }
        }
        intent = intent.with_meta("role", self.config.role.as_str());
        let outcome = run_notify_strict(intent).map_err(|message| internal_error(&message))?;
        ok_json(&outcome)
    }
}

#[tool_handler]
impl ServerHandler for PhasegentMcpServer {
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    /// Protocol-level `tools/list`. The rmcp default advertises every routed
    /// tool; this filters the router descriptors through the shared descriptor
    /// table and its registry-backed role gate, so a client sees the same
    /// allowlist as the `capabilities` payload and role-specific `--help mcp`.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28);
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: Self::tool_router()
                .list_all()
                .into_iter()
                .filter(|tool| self.tool_allowed_for_role(tool.name.as_ref()))
                .collect(),
            meta: None,
            next_cursor: None,
            ttl_ms: supports_cache_hints.then_some(0),
            cache_scope: supports_cache_hints.then_some(CacheScope::Public),
        })
    }

    /// Protocol-level `tools/call`. An unadvertised tool is rejected before
    /// the router deserializes arguments or any handler or provider runs, so
    /// the protocol surface never dispatches a tool the startup role may not
    /// call; the per-handler [`PhasegentMcpServer::require_tool`] gates stay
    /// as defense in depth, and a name with no route still resolves through
    /// the router's not-found path.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if let Some(denied) = tool_registry::TOOLS
            .iter()
            .find(|tool| tool.name == request.name.as_ref())
            .filter(|tool| !tool.allows_role(self.config.role))
        {
            let operation = denied
                .capability()
                .expect("a role-denied tool is capability-gated")
                .operation();
            return Err(permission_error(self.config.role, operation));
        }
        let call = ToolCallContext::new(self, request, context);
        Self::tool_router().call(call).await
    }

    /// Tool definitions resolve through the same allowlist, so the HTTP
    /// transport's `Mcp-Param-*` schema lookups never resolve an unadvertised
    /// tool; an unknown name keeps returning `None`.
    fn get_tool(&self, name: &str) -> Option<Tool> {
        if !self.tool_allowed_for_role(name) {
            return None;
        }
        Self::tool_router().get(name).cloned()
    }
}

/// Tools allowed for a role, derived from the shared MCP descriptor table and
/// the registry-backed capability gates. Mirrors CLI policy without exposing
/// `status_advance`, timers, worktree lease mutations, admin, issue writes,
/// comment reads, hooks, or plugin operations.
pub fn allowed_tools(role: Role) -> Vec<&'static str> {
    tool_registry::TOOLS
        .iter()
        .filter(|tool| tool.allows_role(role))
        .map(|tool| tool.name)
        .collect()
}

/// MCP `phase` metadata must never carry a raw notify setting name.
/// Uses the committed notify predicates so the former dead-code
/// warnings stay resolved and future metadata extensions inherit
/// the same boundary.
fn reject_notify_setting_phase(phase: &str) -> Result<(), McpError> {
    let trimmed = phase.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if crate::notifications::is_notify_secret(trimmed) {
        return Err(invalid_params(
            "notify_send phase must not be a notify secret name",
        ));
    }
    if crate::notifications::is_notify_setting(trimmed) {
        return Err(invalid_params(
            "notify_send phase must not be a notify setting name",
        ));
    }
    Ok(())
}

/// Strict notify delivery on a fresh OS thread. The committed
/// `fire_notification_result` builds its own current-thread runtime
/// and panics inside a Tokio runtime, so MCP tools (which always
/// run inside the rmcp runtime) offload to `std::thread` where no
/// runtime handle exists. Storage is opened on that thread from
/// `PHASEGENT_DB_PATH` so no `!Send` handle crosses threads.
fn run_notify_strict(
    intent: crate::notifications::NotificationIntent,
) -> Result<serde_json::Value, String> {
    let handle = std::thread::spawn(move || {
        let storage = crate::infra::storage::Storage::open().map_err(|message| message.clone())?;
        match crate::notifications::fire_notification_result(&storage, &intent) {
            Ok(outcome) => {
                if outcome.delivered {
                    Ok(serde_json::json!({
                        "notified": true,
                        "event": intent.event.as_str(),
                        "channel": outcome.channel,
                        "notification_id": outcome.row_id,
                    }))
                } else if outcome.channel == "none" {
                    Ok(serde_json::json!({
                        "notified": false,
                        "event": intent.event.as_str(),
                        "channel": "none",
                        "notification_id": outcome.row_id,
                        "skipped": true,
                    }))
                } else {
                    Err(outcome
                        .warning
                        .unwrap_or_else(|| "delivery failed".to_owned()))
                }
            }
            Err(message) => Err(message),
        }
    });
    handle
        .join()
        .map_err(|_| "notification thread panicked".to_owned())?
}

fn ok_json(payload: &serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_owned()),
    )]))
}

fn ok_json_value(payload: &serde_json::Value) -> Result<CallToolResult, McpError> {
    ok_json(payload)
}

fn bound_message(raw: &str) -> String {
    const LIMIT: usize = 300;
    let single = raw.replace(['\n', '\r'], " ");
    let cleaned: String = single.chars().filter(|c| !c.is_control()).collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= LIMIT {
        return collapsed;
    }
    let keep = LIMIT.saturating_sub(3);
    format!("{}...", collapsed.chars().take(keep).collect::<String>())
}

fn internal_error(raw: &str) -> McpError {
    McpError::internal_error(bound_message(raw), None)
}

fn invalid_params(raw: &str) -> McpError {
    McpError::invalid_params(bound_message(raw), None)
}

fn not_supported(provider: &str, operation: &str) -> McpError {
    McpError::internal_error(
        bound_message(&format!("{provider} does not support {operation}")),
        None,
    )
}

fn permission_error(role: Role, operation: &str) -> McpError {
    McpError::internal_error(
        bound_message(&format!(
            "role '{}' is not allowed to perform {operation}",
            role.as_str()
        )),
        None,
    )
}

#[cfg(test)]
#[path = "tool_gate_tests.rs"]
mod tests;
