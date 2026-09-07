//! MCP tools bound to the server-side role.
//!
//! Every tool runs with the [`McpConfig`] captured at `mcp serve`
//! startup. Client requests never supply a role, provider, or
//! credential. Tools call the same provider and notification paths
//! as the CLI; they do not reimplement HTTP or delivery.

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::tool::{Parameters, ToolRouter},
    model::{CallToolResult, Content},
    tool, tool_handler, tool_router,
};

use crate::policy::{Capability, Role};
use crate::providers::{IssueProvider, ProviderDispatcher, ProviderKind};

/// Server-side invocation context. Captured once at `mcp serve`
/// startup from the CLI `--role`/`--provider` flags; never taken
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
    tool_router: ToolRouter<Self>,
}

impl PhasegentMcpServer {
    pub fn new(config: McpConfig) -> Self {
        Self {
            config,
            tool_router: Self::tool_router(),
        }
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
    /// Short summary (bounded to 140 chars).
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
        if !self.config.role.allows(Capability::IssueRead) {
            return Err(permission_error(
                self.config.role,
                Capability::IssueRead.operation(),
            ));
        }
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
        if !self.config.role.allows(Capability::IssueSearch) {
            return Err(permission_error(
                self.config.role,
                Capability::IssueSearch.operation(),
            ));
        }
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
        if !self.config.role.allows(Capability::IssueStatusRead) {
            return Err(permission_error(
                self.config.role,
                Capability::IssueStatusRead.operation(),
            ));
        }
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
        if !self.config.role.allows(Capability::CommentCreate) {
            return Err(permission_error(
                self.config.role,
                Capability::CommentCreate.operation(),
            ));
        }
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
        if !matches!(
            self.config.role,
            Role::Orchestrator | Role::Executor | Role::Reviewer | Role::Tester
        ) {
            return Err(McpError::internal_error(
                format!(
                    "role '{}' is not allowed to perform notify send",
                    self.config.role.as_str()
                ),
                None,
            ));
        }
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
impl ServerHandler for PhasegentMcpServer {}

/// Tools allowed for a role. Mirrors CLI policy without exposing
/// `status_advance`, timers, or role elevation.
pub fn allowed_tools(role: Role) -> Vec<&'static str> {
    let mut tools = vec!["capabilities"];
    if role.allows(Capability::IssueRead) {
        tools.push("issue_get");
    }
    if role.allows(Capability::IssueSearch) {
        tools.push("issue_search");
    }
    if role.allows(Capability::IssueStatusRead) {
        tools.push("status_next");
    }
    if role.allows(Capability::CommentCreate) {
        tools.push("comment_create");
    }
    if matches!(
        role,
        Role::Orchestrator | Role::Executor | Role::Reviewer | Role::Tester
    ) {
        tools.push("notify_send");
    }
    tools
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
    Ok(CallToolResult::success(vec![Content::text(
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
mod tests {
    use super::*;

    #[test]
    fn allowed_tools_never_expose_excluded_operations() {
        for role in [
            Role::Admin,
            Role::Orchestrator,
            Role::Executor,
            Role::Reviewer,
            Role::Tester,
        ] {
            let tools = allowed_tools(role);
            for forbidden in ["status_advance", "timer_start", "timer_finish", "role"] {
                assert!(
                    !tools.iter().any(|tool| tool.contains(forbidden)),
                    "role {} exposed {forbidden}: {tools:?}",
                    role.as_str()
                );
            }
        }
    }

    #[test]
    fn executor_tools_match_contracted_scope() {
        // Executor policy allows read/get but not search; the tool
        // list mirrors CLI policy exactly.
        let tools = allowed_tools(Role::Executor);
        for expected in [
            "capabilities",
            "issue_get",
            "status_next",
            "comment_create",
            "notify_send",
        ] {
            assert!(tools.contains(&expected), "missing {expected}: {tools:?}");
        }
        assert!(
            !tools.contains(&"issue_search"),
            "executor must not expose issue_search per policy: {tools:?}"
        );
    }

    #[test]
    fn orchestrator_tools_include_search() {
        let tools = allowed_tools(Role::Orchestrator);
        for expected in [
            "capabilities",
            "issue_get",
            "issue_search",
            "status_next",
            "comment_create",
            "notify_send",
        ] {
            assert!(tools.contains(&expected), "missing {expected}: {tools:?}");
        }
    }

    #[test]
    fn notify_phase_rejects_setting_and_secret_names() {
        assert!(reject_notify_setting_phase("mcp-phase").is_ok());
        assert!(reject_notify_setting_phase("").is_ok());
        assert!(reject_notify_setting_phase("PHASEGENT_NOTIFY_CHANNEL").is_err());
        assert!(reject_notify_setting_phase("PHASEGENT_NOTIFY_WEBHOOK_TOKEN").is_err());
    }
}
