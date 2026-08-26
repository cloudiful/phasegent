use serde::{Deserialize, Serialize};
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

impl CommentOutput {
    pub fn from_api(comment: ApiComment, marker: Option<String>, include_body: bool) -> Self {
        let marker = marker.or_else(|| marker_from_body(&comment.body));
        Self {
            id: comment.id,
            html_url: comment.html_url,
            marker,
            body: include_body.then_some(comment.body),
        }
    }
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

#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub state: String,
    pub html_url: Option<String>,
}

impl From<ApiIssue> for IssueSummary {
    fn from(issue: ApiIssue) -> Self {
        Self {
            id: issue.id,
            number: issue.number,
            title: issue.title,
            body: issue.body,
            state: issue.state,
            html_url: issue.html_url,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiComment {
    pub id: u64,
    pub body: String,
    pub html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiRepository {
    pub name: String,
    pub full_name: Option<String>,
    pub owner: Option<ApiRepositoryOwner>,
    #[serde(default)]
    pub private: bool,
    pub clone_url: Option<String>,
    pub ssh_url: Option<String>,
    pub html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiRepositoryOwner {
    pub login: Option<String>,
    pub username: Option<String>,
    pub name: Option<String>,
}

impl ApiRepository {
    pub(crate) fn into_summary(self, fallback_owner: &str) -> RepoSummary {
        let owner = self
            .owner
            .and_then(|owner| owner.login.or(owner.username).or(owner.name))
            .unwrap_or_else(|| fallback_owner.to_owned());
        let full_name = self
            .full_name
            .unwrap_or_else(|| format!("{owner}/{}", self.name));
        RepoSummary {
            full_name,
            owner,
            name: self.name,
            private: self.private,
            clone_url: self.clone_url,
            ssh_url: self.ssh_url,
            html_url: self.html_url,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct NewIssue<'a> {
    pub title: &'a str,
    pub body: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct UpdateIssue<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct NewComment<'a> {
    pub body: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct NewRepository<'a> {
    pub name: &'a str,
    pub private: bool,
    pub description: &'a str,
    pub auto_init: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiError {
    pub message: Option<String>,
}

fn marker_from_body(body: &str) -> Option<String> {
    let start = body.find("<!--")?;
    let end = body[start..].find("-->")? + start + 3;
    Some(body[start..end].to_owned())
}
