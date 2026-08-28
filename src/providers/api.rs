use serde::Serialize;
use std::fmt;

#[derive(Debug, Serialize)]
pub struct IssueSummary {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub html_url: Option<String>,
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
