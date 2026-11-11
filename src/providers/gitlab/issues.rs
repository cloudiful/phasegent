use reqwest::Method;
use serde_json::json;

use super::{
    GitlabProvider,
    model::{Issue, Project},
    status::{CLOSED, native_state_name},
};
use crate::policy::Capability;
use crate::providers::api::{
    IssueProjectRef, IssueSearchItem, IssueSearchOptions, IssueSearchResult, IssueSummary,
    IssueSummaryPage, PhasegentError,
};
use crate::providers::{IssueProvider, ProviderCapabilities};

impl GitlabProvider {
    pub(super) fn summary(
        &self,
        issue: Issue,
        project: &Project,
        expected: Option<u64>,
    ) -> Result<IssueSummary, PhasegentError> {
        if issue.id == 0 || issue.iid == 0 || expected.is_some_and(|iid| iid != issue.iid) {
            return Err(PhasegentError::config("GitLab issue identity mismatch"));
        }
        let state = native_state_name(&issue.state)?;
        Ok(IssueSummary {
            id: issue.id,
            number: issue.iid,
            title: issue.title,
            body: issue.description.unwrap_or_default(),
            state: if state == CLOSED { "closed" } else { "open" }.to_owned(),
            html_url: issue.web_url,
            project: Some(IssueProjectRef {
                id: project.id,
                name: project.name.clone(),
                identifier: Some(project.path_with_namespace.clone()),
            }),
        })
    }

    pub(super) fn load_issue(
        &self,
        iid: u64,
        project: &Project,
    ) -> Result<IssueSummary, PhasegentError> {
        if iid == 0 {
            return Err(PhasegentError::config("issue number must be positive"));
        }
        let index = iid.to_string();
        let issue: Issue = self.http.get(
            self.http.request(
                Method::GET,
                &["projects", self.config.repository(), "issues", &index],
            )?,
            "issue get",
        )?;
        self.summary(issue, project, Some(iid))
    }

    fn issue_page(&self, options: &IssueSearchOptions) -> Result<IssueSummaryPage, PhasegentError> {
        options.validate()?;
        let project = self.project()?;
        let state = match options.state.as_str() {
            "open" => "opened",
            "closed" => "closed",
            "all" => "all",
            _ => {
                return Err(PhasegentError::config(
                    "issue state must be open, closed, or all",
                ));
            }
        };
        let mut query = vec![("state", state.to_owned()), ("scope", "all".to_owned())];
        if let Some(value) = options.effective_query() {
            query.push(("search", value.to_owned()));
        }
        let issues: Vec<Issue> = self.collect(
            &["projects", self.config.repository(), "issues"],
            &query,
            "issue search",
        )?;
        let items = issues
            .into_iter()
            .map(|issue| self.summary(issue, &project, None))
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

impl GitlabProvider {
    pub(crate) const CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
        issue_lifecycle: true,
        comments: true,
        project_discovery: false,
        legacy_numeric_leases: false,
    };
}

impl IssueProvider for GitlabProvider {
    type Error = PhasegentError;
    fn capabilities(&self) -> ProviderCapabilities {
        Self::CAPABILITIES
    }
    fn supports(&self, capability: Capability) -> bool {
        super::capabilities::supports(capability)
    }
    fn get_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        self.load_issue(number, &self.project()?)
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
                .map(|value| IssueSearchItem::from_summary(value, options.include_body))
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
        let project = self.project()?;
        let request = self
            .http
            .request(
                Method::POST,
                &["projects", self.config.repository(), "issues"],
            )?
            .json(&json!({"title": title, "description": body}));
        let issue: Issue = self.http.write(request, "issue create")?;
        self.summary(issue, &project, None)
    }
    fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, Self::Error> {
        let project = self.project()?;
        self.load_issue(number, &project)?;
        let index = number.to_string();
        let request = self
            .http
            .request(
                Method::PUT,
                &["projects", self.config.repository(), "issues", &index],
            )?
            .json(&json!({"description": body}));
        let issue: Issue = self.http.write(request, "issue update")?;
        self.summary(issue, &project, Some(number))
    }
    fn close_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        self.transition_status(number, CLOSED)
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
