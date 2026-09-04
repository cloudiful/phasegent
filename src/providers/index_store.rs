// Provider-neutral index search and scope helpers.
// Extracted to keep `providers::index` cohesive and below size thresholds.

use serde::Serialize;

use crate::providers::api::ISSUE_SEARCH_MAX_BODY_BYTES;
use crate::providers::api::truncate_to_byte_limit;

/// Bounded envelope returned by `issue index search`.
#[derive(Debug, Serialize, Clone)]
pub struct IssueIndexSearchItem {
    pub source: String,
    pub project: String,
    pub external_id: String,
    pub issue_number: u64,
    pub title: String,
    pub state: String,
    pub html_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_truncated: Option<bool>,
}

impl IssueIndexSearchItem {
    pub fn from_document(
        doc: &crate::providers::index::IssueIndexDocument,
        include_body: bool,
    ) -> Self {
        if !include_body {
            Self {
                source: doc.key.source.clone(),
                project: doc.key.project.clone(),
                external_id: doc.key.external_id.clone(),
                issue_number: doc.issue_number,
                title: doc.title.clone(),
                state: doc.state.clone(),
                html_url: doc.url.clone(),
                body: None,
                body_truncated: None,
            }
        } else {
            let truncated = doc.body.len() > ISSUE_SEARCH_MAX_BODY_BYTES;
            let body = if truncated {
                truncate_to_byte_limit(&doc.body, ISSUE_SEARCH_MAX_BODY_BYTES).to_owned()
            } else {
                doc.body.clone()
            };
            Self {
                source: doc.key.source.clone(),
                project: doc.key.project.clone(),
                external_id: doc.key.external_id.clone(),
                issue_number: doc.issue_number,
                title: doc.title.clone(),
                state: doc.state.clone(),
                html_url: doc.url.clone(),
                body: Some(body),
                body_truncated: Some(truncated),
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct IssueIndexSearchResult {
    pub items: Vec<IssueIndexSearchItem>,
    pub offset: usize,
    pub limit: usize,
    pub total_count: usize,
    pub has_more: bool,
}

/// Deterministic scope that keys every index document.
/// `source` is the provider kind literal (`forgejo`/`redmine`/`gitlab`),
/// `project` is the stable project identifier for that provider.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct IssueIndexScope {
    pub source: String,
    pub project: String,
}

impl IssueIndexScope {
    pub fn new(source: impl Into<String>, project: impl Into<String>) -> Result<Self, String> {
        let source = source.into();
        let project = project.into();
        if source.trim().is_empty() || project.trim().is_empty() {
            return Err("scope source and project must be non-empty".to_owned());
        }
        Ok(Self {
            source: source.trim().to_owned(),
            project: project.trim().to_owned(),
        })
    }
}

/// Derive a stable scope from a resolved dispatcher.
/// Forgejo uses `owner/repo`, Redmine uses the explicit project id (required),
/// GitLab uses the numeric project id as a string. Returns a config error
/// when Redmine project id is missing so callers never silently index all
/// projects.
pub fn provider_scope(
    dispatcher: &crate::providers::ProviderDispatcher,
) -> Result<IssueIndexScope, crate::providers::api::ForgejoError> {
    use crate::providers::api::ForgejoError;
    match dispatcher {
        crate::providers::ProviderDispatcher::Forgejo(provider) => {
            let project = format!("{}/{}", provider.config.owner, provider.config.repository);
            Ok(IssueIndexScope {
                source: "forgejo".to_owned(),
                project,
            })
        }
        crate::providers::ProviderDispatcher::Redmine(provider) => {
            let project = provider
                .config
                .project_id
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| {
                    ForgejoError::config(
                        "Redmine project id is required for issue index sync; use --project-id",
                    )
                })?;
            Ok(IssueIndexScope {
                source: "redmine".to_owned(),
                project: project.trim().to_owned(),
            })
        }
        crate::providers::ProviderDispatcher::Gitlab(provider) => Ok(IssueIndexScope {
            source: "gitlab".to_owned(),
            project: provider.config.project_id.to_string(),
        }),
    }
}

/// Summary returned by `issue index sync`.
#[derive(Debug, Serialize)]
pub struct IssueIndexSyncSummary {
    pub source: String,
    pub project: String,
    pub pages_synced: usize,
    pub indexed: usize,
    pub tombstoned: usize,
    pub has_more: bool,
    pub completed: bool,
    pub limit: usize,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}
