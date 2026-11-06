use reqwest::Method;
use serde_json::json;

use super::{
    ForgejoProvider,
    model::{Issue, Repository},
};
use crate::policy::Capability;
use crate::providers::api::{
    IssueProjectRef, IssueSearchItem, IssueSearchOptions, IssueSearchResult, IssueSummary,
    IssueSummaryPage, PhasegentError,
};
use crate::providers::{IssueProvider, ProviderCapabilities};

impl ForgejoProvider {
    pub(super) fn summary(
        &self,
        issue: Issue,
        repo: &Repository,
        expected: Option<u64>,
    ) -> Result<IssueSummary, PhasegentError> {
        if issue.pull_request.is_some() {
            return Err(PhasegentError::config(
                "Forgejo response is a pull request, not an issue",
            ));
        }
        if issue.id == 0 || issue.number == 0 || expected.is_some_and(|n| n != issue.number) {
            return Err(PhasegentError::config("Forgejo issue identity mismatch"));
        }
        if let Some(associated) = &issue.repository {
            self.validate_repository(associated)?;
            if associated.id != repo.id {
                return Err(PhasegentError::config(
                    "Forgejo issue repository identity mismatch",
                ));
            }
        }
        if !matches!(issue.state.as_str(), "open" | "closed") {
            return Err(PhasegentError::config("invalid Forgejo issue state"));
        }
        Ok(IssueSummary {
            id: issue.id,
            number: issue.number,
            title: issue.title,
            body: issue.body.unwrap_or_default(),
            state: issue.state,
            html_url: issue.html_url,
            project: Some(IssueProjectRef {
                id: repo.id,
                name: repo.name.clone(),
                identifier: Some(repo.full_name.clone()),
            }),
        })
    }

    pub(super) fn load_issue(
        &self,
        number: u64,
        repo: &Repository,
    ) -> Result<IssueSummary, PhasegentError> {
        if number == 0 {
            return Err(PhasegentError::config("issue number must be positive"));
        }
        let index = number.to_string();
        let issue = self.http.get(
            self.http.request(Method::GET, &["issues", &index])?,
            "issue get",
        )?;
        self.summary(issue, repo, Some(number))
    }

    fn issue_page(&self, options: &IssueSearchOptions) -> Result<IssueSummaryPage, PhasegentError> {
        options.validate()?;
        let repo = self.repository()?;
        let mut query = vec![
            ("type", "issues".to_owned()),
            ("state", options.state.clone()),
        ];
        if let Some(value) = options.effective_query() {
            query.push(("q", value.to_owned()));
        }
        let issues: Vec<Issue> = self.collect(&["issues"], &query, "issue search")?;
        let items = issues
            .into_iter()
            .map(|issue| self.summary(issue, &repo, None))
            .collect::<Result<Vec<_>, _>>()?;
        let total = items.len();
        let offset = (options.page - 1)
            .checked_mul(options.limit)
            .ok_or_else(|| PhasegentError::config("issue search offset is too large"))?;
        let items: Vec<_> = items.into_iter().skip(offset).take(options.limit).collect();
        Ok(IssueSummaryPage {
            has_more: offset.saturating_add(items.len()) < total,
            items,
            page: options.page,
            limit: options.limit,
            total_count: Some(total),
        })
    }
}

impl ForgejoProvider {
    /// The provider's fixed capability facts, available without building a
    /// credentialed provider so selection and lifecycle decisions stay local.
    pub(crate) const CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
        issue_lifecycle: true,
        comments: true,
        project_discovery: false,
        legacy_numeric_leases: false,
    };
}

impl IssueProvider for ForgejoProvider {
    type Error = PhasegentError;
    fn capabilities(&self) -> ProviderCapabilities {
        Self::CAPABILITIES
    }
    fn supports(&self, capability: Capability) -> bool {
        super::capabilities::supports(capability)
    }
    fn get_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        self.load_issue(number, &self.repository()?)
    }
    fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, Self::Error> {
        let page = self.issue_page(options)?;
        Ok(IssueSearchResult {
            items: page
                .items
                .into_iter()
                .map(|v| IssueSearchItem::from_summary(v, options.include_body))
                .collect(),
            page: page.page,
            limit: page.limit,
            total_count: page.total_count,
            has_more: page.has_more,
        })
    }
    fn search_issue_page(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSummaryPage, Self::Error> {
        self.issue_page(options)
    }
    fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, Self::Error> {
        let repo = self.repository()?;
        // The native issue starts open: no status label is required, read or
        // sent, so no label catalogue can block a create.
        let request = self
            .http
            .request(Method::POST, &["issues"])?
            .json(&json!({"title": title, "body": body}));
        let issue = self.http.write(request, "issue create")?;
        self.summary(issue, &repo, None)
    }
    fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, Self::Error> {
        let repo = self.repository()?;
        self.load_issue(number, &repo)?;
        let index = number.to_string();
        let request = self
            .http
            .request(Method::PATCH, &["issues", &index])?
            .json(&json!({"body": body}));
        let issue = self.http.write(request, "issue update")?;
        self.summary(issue, &repo, Some(number))
    }
    fn close_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        self.transition_status(number, "Closed")
    }
    fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<crate::providers::api::CommentOutput, Self::Error> {
        self.create_issue_comment(issue, body, marker)
    }
    fn get_comment(
        &self,
        issue: u64,
        comment: u64,
    ) -> Result<crate::providers::api::CommentOutput, Self::Error> {
        self.get_issue_comment(issue, comment)
    }
    fn find_marker(
        &self,
        issue: u64,
        marker: &str,
    ) -> Result<crate::providers::api::CommentOutput, Self::Error> {
        self.find_issue_marker(issue, marker)
    }
    fn list_comments(
        &self,
        issue: u64,
    ) -> Result<Vec<crate::providers::api::CommentOutput>, Self::Error> {
        self.issue_comments(issue)
    }
    fn list_comments_page(
        &self,
        issue: u64,
        page: crate::pagination::PageRequest,
    ) -> Result<crate::pagination::Paged<crate::providers::api::CommentOutput>, Self::Error> {
        Ok(crate::pagination::slice(self.issue_comments(issue)?, page))
    }
}
