use crate::providers::api::{
    ForgejoError, IssueSearchItem, IssueSearchOptions, IssueSearchResult, IssueSummary,
};
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{
    IssuePlanning, RedmineIssue, RedmineIssueCollection, RedmineIssueResponse, RedmineNewIssue,
    RedmineUpdateIssue,
};

impl RedmineProvider {
    pub fn get_issue(&self, number: u64) -> Result<IssueSummary, ForgejoError> {
        let issue = self.issue_with_journals(number, "issue get")?;
        Ok(self.issue_summary(issue))
    }

    pub fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, ForgejoError> {
        options.validate()?;
        let project_id = self
            .config
            .project_id
            .as_deref()
            .filter(|project_id| !project_id.trim().is_empty())
            .map(str::to_owned);
        let status_id = match options.state.as_str() {
            "open" => "open",
            "closed" => "closed",
            "all" => "*",
            _ => {
                return Err(ForgejoError::config(
                    "issue state must be open, closed, or all",
                ));
            }
        };
        let offset = (options.page.saturating_sub(1)).saturating_mul(options.limit);
        let mut params = vec![
            ("status_id", status_id.to_owned()),
            ("limit", options.limit.to_string()),
            ("offset", offset.to_string()),
        ];
        if let Some(project_id) = &project_id {
            params.push(("project_id", project_id.clone()));
        }
        if let Some(query) = options.effective_query() {
            params.push(("subject", format!("~{query}")));
        }
        let page: RedmineIssueCollection = self.http.get("issues.json", &params, "issue search")?;
        let total_count = page.total_count;
        let count = page.issues.len();
        let has_more = if let Some(total) = total_count {
            offset + count < total
        } else {
            count == options.limit
        };
        let items: Vec<IssueSearchItem> = page
            .issues
            .into_iter()
            .filter(|issue| issue.matches_state(&options.state))
            .map(|issue| {
                let summary = self.issue_summary(issue);
                IssueSearchItem::from_summary(summary, options.include_body)
            })
            .collect();
        Ok(IssueSearchResult {
            items,
            page: options.page,
            limit: options.limit,
            total_count,
            has_more,
        })
    }

    pub fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, ForgejoError> {
        self.create_issue_with_planning(title, body, None, &IssuePlanning::default())
    }

    /// Create an issue with an explicit tracker id already resolved through
    /// [`RedmineProvider::select_tracker`].
    pub fn create_issue_with_tracker(
        &self,
        title: &str,
        body: &str,
        tracker_id: u64,
    ) -> Result<IssueSummary, ForgejoError> {
        self.create_issue_with_planning(title, body, Some(tracker_id), &IssuePlanning::default())
    }

    /// Create an issue with an optional tracker plus native planning
    /// fields. Fields absent from `planning` stay out of the JSON payload
    /// so the legacy create request shape is preserved.
    pub fn create_issue_with_planning(
        &self,
        title: &str,
        body: &str,
        tracker_id: Option<u64>,
        planning: &IssuePlanning,
    ) -> Result<IssueSummary, ForgejoError> {
        let project_id = self.config.require_project_id()?;
        let payload = RedmineNewIssue::new(project_id, title, body)
            .with_tracker_option(tracker_id)
            .with_planning(planning);
        let response: RedmineIssueResponse =
            self.http.post("issues.json", &payload, "issue create")?;
        Ok(self.issue_summary(response.issue))
    }

    pub fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, ForgejoError> {
        let payload = RedmineUpdateIssue::description(body);
        self.put_issue_update(number, payload, "issue update-body")
    }

    /// Update the body and re-target the tracker in a single PUT. The
    /// tracker id must already be resolved through
    /// [`RedmineProvider::select_tracker`].
    pub fn update_body_with_tracker(
        &self,
        number: u64,
        body: &str,
        tracker_id: u64,
    ) -> Result<IssueSummary, ForgejoError> {
        let payload = RedmineUpdateIssue::description_with_tracker(body, tracker_id);
        self.put_issue_update(number, payload, "issue update-body")
    }

    /// Update the body with an optional tracker re-target plus native
    /// planning fields in one atomic PUT. Fields absent from `planning`
    /// stay out of the JSON payload so the legacy update request shape is
    /// preserved.
    pub fn update_body_with_planning(
        &self,
        number: u64,
        body: &str,
        tracker_id: Option<u64>,
        planning: &IssuePlanning,
    ) -> Result<IssueSummary, ForgejoError> {
        let payload = match tracker_id {
            Some(tracker_id) => RedmineUpdateIssue::description_with_tracker(body, tracker_id),
            None => RedmineUpdateIssue::description(body),
        }
        .with_planning(planning);
        self.put_issue_update(number, payload, "issue update-body")
    }

    fn put_issue_update(
        &self,
        number: u64,
        payload: RedmineUpdateIssue<'_>,
        operation: &'static str,
    ) -> Result<IssueSummary, ForgejoError> {
        let response: Option<RedmineIssueResponse> =
            self.http
                .put(&self.issue_path(number), &payload, operation)?;
        response
            .map(|response| self.issue_summary(response.issue))
            .map_or_else(|| self.get_issue(number), Ok)
    }

    pub(crate) fn issue_with_journals(
        &self,
        number: u64,
        operation: &str,
    ) -> Result<RedmineIssue, ForgejoError> {
        let params = [("include", "journals".to_owned())];
        let response: RedmineIssueResponse =
            self.http
                .get(&self.issue_path(number), &params, operation)?;
        Ok(response.issue)
    }

    pub(crate) fn issue_path(&self, number: u64) -> String {
        format!("issues/{number}.json")
    }

    pub(crate) fn issue_summary(&self, issue: RedmineIssue) -> IssueSummary {
        let url = self.http.issue_url(issue.id);
        issue.into_summary(url)
    }
}
