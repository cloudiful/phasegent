//! Issue CRUD / search and IssueSummary conversion.

use crate::providers::api::{
    ForgejoError, IssueSearchItem, IssueSearchOptions, IssueSearchResult, IssueSummary,
};
use crate::providers::gitlab::model::{
    ApiIssue, NewIssue, UpdateIssue, state_from_gitlab, state_query_filter,
};

use super::core::GitlabProvider;

// -- Bridge helper ---------------------------------------------------------

/// Some GitLab endpoints return the updated resource as the response
/// body, others return `200 OK` with no body. Decode the optional
/// body and surface a structured error when it is missing.
pub(crate) fn parse_optional_issue(
    option: Option<ApiIssue>,
    operation: &'static str,
) -> Result<ApiIssue, ForgejoError> {
    option.ok_or_else(|| {
        ForgejoError::not_found(operation, "GitLab did not return the updated issue")
    })
}

impl GitlabProvider {
    /// `GET /projects/:id/issues/:iid` - one issue by its project-
    /// scoped `iid`.
    pub(crate) fn get_issue(&self, iid: u64) -> Result<IssueSummary, ForgejoError> {
        let issue: ApiIssue = self.http.get(&self.issue_path(iid), &[], "issue get")?;
        Ok(issue.into_summary(self))
    }

    /// `GET /projects/:id/issues/:iid` returning the raw `ApiIssue`
    /// so callers that must inspect the full label set (the
    /// [`IssueSummary`] view strips it) can do so without a second
    /// network call. Used by the label-replacement path in
    /// [`Self::update_body_with_labels`] to
    /// detect the opposite managed tracker label.
    pub(crate) fn get_raw_issue(&self, iid: u64) -> Result<ApiIssue, ForgejoError> {
        self.http.get(&self.issue_path(iid), &[], "issue get")
    }

    /// `GET /projects/:id/issues?state=...&search=...&per_page=50&page=N`
    /// single-page fetch. Bounded: never loops across pages; the caller
    /// controls pagination via `page`/`per_page`. State filter is the
    /// GitLab `opened`/`closed`/omitted translation.
    pub(crate) fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, ForgejoError> {
        options.validate()?;
        let state_filter = state_query_filter(&options.state)?;
        let path = self.issues_path();
        let query = options.effective_query();
        let mut params = vec![
            ("page", options.page.to_string()),
            ("per_page", options.limit.to_string()),
        ];
        if let Some(filter) = state_filter {
            params.push(("state", filter.to_owned()));
        }
        if let Some(query) = query {
            params.push(("search", query.to_owned()));
        }
        let (items, headers, _raw) = self.http.get_page::<ApiIssue>(&path, &params, "issue search")?;
        let count = items.len();
        // GitLab signals more pages via x-next-page (empty = last page)
        // and x-total-pages / x-total. Preserve that metadata.
        let total_count = headers
            .get("x-total")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<usize>().ok());
        let next_page_header = headers
            .get("x-next-page")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_owned());
        let total_pages_header = headers
            .get("x-total-pages")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<usize>().ok());
        let offset = (options.page.saturating_sub(1)).saturating_mul(options.limit);
        let has_more = match (next_page_header.as_deref(), total_pages_header, total_count) {
            (Some(""), _, _) => false,
            (Some(_), Some(total_pages), _) => options.page < total_pages,
            (_, _, Some(total)) => offset + count < total,
            (None, Some(total_pages), None) => options.page < total_pages,
            (None, None, None) => count == options.limit,
            (Some(_), None, None) => true,
        };
        let items: Vec<IssueSearchItem> = items
            .into_iter()
            .map(|issue| {
                let summary: IssueSummary = issue.into_summary(self);
                IssueSearchItem::from_summary(summary, options.include_body)
            })
            .collect();
        Ok(IssueSearchResult {
            items,
            page: options.page,
            limit: options.limit,
            total_count,
            has_more: has_more && count > 0,
        })
    }

    /// `POST /projects/:id/issues` with an optional `labels` field
    /// for tracker mapping.
    pub(crate) fn create_issue_with_labels(
        &self,
        title: &str,
        description: &str,
        labels: &[String],
    ) -> Result<IssueSummary, ForgejoError> {
        let payload = NewIssue {
            title,
            description,
            labels: labels.to_vec(),
        };
        let issue: ApiIssue = self
            .http
            .post(&self.issues_path(), &payload, "issue create")?;
        Ok(issue.into_summary(self))
    }

    /// Plain create without label manipulation. Mirrors the shared
    /// `IssueProvider::create_issue` signature so the trait impl can
    /// delegate cleanly; the planning-aware CLI path uses
    /// [`Self::create_issue_with_labels`] directly when a tracker is
    /// supplied.
    pub(crate) fn create_issue(
        &self,
        title: &str,
        body: &str,
    ) -> Result<IssueSummary, ForgejoError> {
        self.create_issue_with_labels(title, body, &[])
    }

    /// `PUT /projects/:id/issues/:iid` with an optional description
    /// and label delta.
    ///
    /// When `labels` includes a managed tracker label (`type::bug` /
    /// `type::feature`), the opposite tracker label is added to
    /// `remove_labels` if the issue currently carries it so the issue
    /// never ends up holding both. Workflow labels and unrelated
    /// project labels are preserved untouched.
    pub(crate) fn update_body_with_labels(
        &self,
        iid: u64,
        description: &str,
        labels: &[String],
    ) -> Result<IssueSummary, ForgejoError> {
        // Ensure every label we are about to add already exists in
        // the project before referencing it; GitLab rejects a PUT
        // for an unknown label.
        if !labels.is_empty() {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            self.ensure_labels(&refs)?;
        }
        // Fetch the raw issue so we can inspect the full label set
        // before deciding which managed tracker label to remove.
        let current = self.get_raw_issue(iid)?;
        let mut remove_labels: Vec<String> = Vec::new();
        for added in labels {
            if let Some(opposite) = Self::opposite_tracker_label(added) {
                let already_added = labels.iter().any(|value| value == opposite);
                let currently_attached = current.labels.iter().any(|value| value == opposite);
                if currently_attached && !already_added {
                    remove_labels.push(opposite.to_owned());
                }
            }
        }
        let payload = UpdateIssue {
            description: Some(description),
            state_event: None,
            add_labels: labels.to_vec(),
            remove_labels,
        };
        let response: Option<ApiIssue> =
            self.http
                .put(&self.issue_path(iid), &payload, "issue update-body")?;
        parse_optional_issue(response, "issue update-body").map(|issue| issue.into_summary(self))
    }

    /// Plain body update; no label delta. Used when a caller only
    /// updates the description and explicitly does not want to
    /// disturb the current label set.
    pub(crate) fn update_body(&self, iid: u64, body: &str) -> Result<IssueSummary, ForgejoError> {
        let payload = UpdateIssue {
            description: Some(body),
            state_event: None,
            add_labels: Vec::new(),
            remove_labels: Vec::new(),
        };
        let response: Option<ApiIssue> =
            self.http
                .put(&self.issue_path(iid), &payload, "issue update-body")?;
        parse_optional_issue(response, "issue update-body").map(|issue| issue.into_summary(self))
    }

    /// Close an issue via the native `state_event=close` field plus
    /// the `workflow::closed` label so the orchestrator's existing
    /// status invariants still hold.
    pub(crate) fn close_issue(&self, iid: u64) -> Result<IssueSummary, ForgejoError> {
        self.apply_status(iid, Some("workflow::closed"), true)
    }
}

impl ApiIssue {
    pub(crate) fn into_summary(self, _provider: &GitlabProvider) -> IssueSummary {
        let state = state_from_gitlab(&self.state).to_owned();
        IssueSummary {
            id: self.id,
            number: self.iid,
            title: self.title,
            body: self.description,
            state,
            html_url: self.web_url,
        }
    }
}
