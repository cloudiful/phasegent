use serde::Serialize;
use std::fmt;

pub const ISSUE_SEARCH_DEFAULT_PAGE: usize = 1;
pub const ISSUE_SEARCH_DEFAULT_LIMIT: usize = 50;
pub const ISSUE_SEARCH_MAX_LIMIT: usize = 100;
pub const ISSUE_SEARCH_MAX_BODY_BYTES: usize = 8192;

#[derive(Debug, Serialize)]
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
            }
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

#[derive(Debug)]
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
