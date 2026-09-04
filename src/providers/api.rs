use serde::Serialize;
use std::fmt;

pub const ISSUE_SEARCH_DEFAULT_PAGE: usize = 1;
pub const ISSUE_SEARCH_DEFAULT_LIMIT: usize = 50;
pub const ISSUE_SEARCH_MAX_LIMIT: usize = 100;
pub const ISSUE_SEARCH_MAX_BODY_BYTES: usize = 8192;

#[derive(Debug, Serialize, Clone)]
pub struct IssueSummary {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub html_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IssueSearchOptions {
    pub query: Option<String>,
    pub state: String,
    pub page: usize,
    pub limit: usize,
    pub include_body: bool,
    pub all: bool,
}

impl IssueSearchOptions {
    pub fn effective_query(&self) -> Option<&str> {
        self.query
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub fn validate(&self) -> Result<(), ForgejoError> {
        if !matches!(self.state.as_str(), "open" | "closed" | "all") {
            return Err(ForgejoError::config(
                "issue state must be open, closed, or all",
            ));
        }
        if self.page == 0 {
            return Err(ForgejoError::config("issue search page must be >= 1"));
        }
        if self.limit == 0 || self.limit > ISSUE_SEARCH_MAX_LIMIT {
            return Err(ForgejoError::config(format!(
                "issue search limit must be between 1 and {ISSUE_SEARCH_MAX_LIMIT}"
            )));
        }
        let has_query = self.effective_query().is_some();
        if !has_query && !self.all {
            return Err(ForgejoError::config(
                "issue search requires --query TEXT or --all for a bounded all-issues listing (empty queries are rejected)",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct IssueSearchItem {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub html_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_truncated: Option<bool>,
    /// Additive local-index scope retained only on stale fallback items.
    /// Provider-fresh results leave all three as `None` so stdout JSON
    /// stays byte-compatible with the pre-Phase-2 shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

pub(crate) fn truncate_to_byte_limit(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        value
    } else {
        let mut end = max_bytes;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        &value[..end]
    }
}

impl IssueSearchItem {
    pub fn from_summary(summary: IssueSummary, include_body: bool) -> Self {
        if !include_body {
            Self {
                id: summary.id,
                number: summary.number,
                title: summary.title,
                state: summary.state,
                html_url: summary.html_url,
                body: None,
                body_truncated: None,
                source: None,
                project: None,
                external_id: None,
            }
        } else {
            let truncated = summary.body.len() > ISSUE_SEARCH_MAX_BODY_BYTES;
            let body = if truncated {
                truncate_to_byte_limit(&summary.body, ISSUE_SEARCH_MAX_BODY_BYTES).to_owned()
            } else {
                summary.body
            };
            Self {
                id: summary.id,
                number: summary.number,
                title: summary.title,
                state: summary.state,
                html_url: summary.html_url,
                body: Some(body),
                body_truncated: Some(truncated),
                source: None,
                project: None,
                external_id: None,
            }
        }
    }

    /// Build a stale-fallback item from local index fields without
    /// inventing numeric ids: `id`/`number` reuse the stored numeric
    /// `issue_number`, while the opaque `external_id` string is retained
    /// verbatim alongside `source`/`project` so consumers can tell the
    /// row is stale and scoped.
    pub fn from_local_parts(
        source: String,
        project: String,
        external_id: String,
        issue_number: u64,
        title: String,
        state: String,
        html_url: Option<String>,
        body_full: String,
        include_body: bool,
    ) -> Self {
        let (body, body_truncated) = if !include_body {
            (None, None)
        } else {
            let truncated = body_full.len() > ISSUE_SEARCH_MAX_BODY_BYTES;
            let body = if truncated {
                truncate_to_byte_limit(&body_full, ISSUE_SEARCH_MAX_BODY_BYTES).to_owned()
            } else {
                body_full
            };
            (Some(body), Some(truncated))
        };
        Self {
            id: issue_number,
            number: issue_number,
            title,
            state,
            html_url,
            body,
            body_truncated,
            source: Some(source),
            project: Some(project),
            external_id: Some(external_id),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct IssueSearchResult {
    pub items: Vec<IssueSearchItem>,
    pub page: usize,
    pub limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<usize>,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct IssueSummaryPage {
    pub items: Vec<IssueSummary>,
    pub page: usize,
    pub limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<usize>,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct CommentOutput {
    pub id: u64,
    pub html_url: Option<String>,
    pub marker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RepoSummary {
    pub full_name: String,
    pub owner: String,
    pub name: String,
    pub private: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clone_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html_url: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ForgejoError {
    Config(String),
    Auth(String),
    Request {
        operation: String,
        message: String,
    },
    Http {
        operation: String,
        status: u16,
        message: String,
    },
    Decode {
        operation: String,
        message: String,
    },
    NotFound {
        operation: String,
        message: String,
    },
    Pagination {
        operation: String,
        message: String,
    },
    NotSupported {
        provider: String,
        operation: String,
        message: String,
    },
}

impl ForgejoError {
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::Auth(message.into())
    }

    /// True for capability errors that must never trigger local fallback.
    /// `not_supported` is permanent, not a transient provider outage.
    pub fn is_not_supported(&self) -> bool {
        matches!(self, Self::NotSupported { .. })
    }

    /// True for argument-shaped config errors that must not be masked by
    /// stale local results. Provider-resolution config errors (missing
    /// base/repository, invalid URL shape) remain fallback-eligible and
    /// return false here; only `issue search` option validation messages
    /// are treated as arguments.
    pub fn is_search_argument_error(&self) -> bool {
        match self {
            Self::Config(message) => {
                let message = message.as_str();
                message.contains("issue search requires")
                    || message.contains("issue state must be")
                    || message.contains("issue search page must be")
                    || message.contains("issue search limit must be")
            }
            _ => false,
        }
    }

    pub(crate) fn request(operation: &str, message: String) -> Self {
        Self::Request {
            operation: operation.to_owned(),
            message,
        }
    }

    pub(crate) fn not_found(operation: &str, message: &str) -> Self {
        Self::NotFound {
            operation: operation.to_owned(),
            message: message.to_owned(),
        }
    }

    pub(crate) fn pagination(operation: &str, message: &str) -> Self {
        Self::Pagination {
            operation: operation.to_owned(),
            message: message.to_owned(),
        }
    }

    pub(crate) fn not_supported(provider: &str, operation: &str) -> Self {
        Self::NotSupported {
            provider: provider.to_owned(),
            operation: operation.to_owned(),
            message: format!("{provider} does not support {operation}"),
        }
    }

    pub fn json(&self) -> serde_json::Value {
        match self {
            Self::Config(message) => serde_json::json!({"kind":"config", "message":message}),
            Self::Auth(message) => serde_json::json!({"kind":"auth", "message":message}),
            Self::Request { operation, message } => {
                serde_json::json!({"kind":"request", "operation":operation, "message":message})
            }
            Self::Http {
                operation,
                status,
                message,
            } => {
                serde_json::json!({"kind":"http", "operation":operation, "status":status, "message":message})
            }
            Self::Decode { operation, message } => {
                serde_json::json!({"kind":"decode", "operation":operation, "message":message})
            }
            Self::NotFound { operation, message } => {
                serde_json::json!({"kind":"not_found", "operation":operation, "message":message})
            }
            Self::Pagination { operation, message } => {
                serde_json::json!({"kind":"pagination", "operation":operation, "message":message})
            }
            Self::NotSupported {
                provider,
                operation,
                message,
            } => {
                serde_json::json!({"kind":"not_supported", "provider":provider, "operation":operation, "message":message})
            }
        }
    }
}

impl fmt::Display for ForgejoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.json().to_string())
    }
}
