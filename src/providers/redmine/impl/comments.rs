use crate::providers::api::{CommentOutput, ForgejoError};
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::RedmineIssueResponse;

impl RedmineProvider {
    pub fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, ForgejoError> {
        if marker.is_empty() {
            return Err(ForgejoError::config("marker cannot be empty"));
        }
        let payload = crate::providers::redmine::model::RedmineNotes {
            issue: crate::providers::redmine::model::RedmineNotesFields { notes: body },
        };
        let response: Option<RedmineIssueResponse> =
            self.http
                .put(&self.issue_path(issue), &payload, "comment create")?;
        let issue = match response {
            Some(response) if response.issue.find_journal(body, marker).is_some() => response.issue,
            _ => self.issue_with_journals(issue, "comment create")?,
        };
        let journal = issue.find_journal(body, marker).ok_or_else(|| {
            ForgejoError::not_found(
                "comment create",
                "Redmine did not return the created journal",
            )
        })?;
        Ok(journal.to_comment(&self.http.issue_url(issue.id), Some(marker), false))
    }

    pub fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, ForgejoError> {
        let issue_data = self.issue_with_journals(issue, "comment get")?;
        let journal = issue_data
            .journals
            .iter()
            .find(|journal| journal.id == comment)
            .ok_or_else(|| {
                ForgejoError::not_found(
                    "comment get",
                    "comment was not found in the specified issue",
                )
            })?;
        Ok(journal.to_comment(&self.http.issue_url(issue_data.id), None, true))
    }

    /// Full bodies of every journal on the issue, in API order.
    /// Backs `comment list` through the trait forwarder.
    pub fn list_comments(&self, issue: u64) -> Result<Vec<CommentOutput>, ForgejoError> {
        let issue_data = self.issue_with_journals(issue, "comment list")?;
        let url = self.http.issue_url(issue_data.id);
        Ok(issue_data
            .journals
            .iter()
            .map(|journal| journal.to_comment(&url, None, true))
            .collect())
    }

    pub fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, ForgejoError> {
        if marker.is_empty() {
            return Err(ForgejoError::config("marker cannot be empty"));
        }
        let issue_data = self.issue_with_journals(issue, "comment find-marker")?;
        let journal = issue_data
            .journals
            .iter()
            .find(|journal| journal.notes.contains(marker))
            .ok_or_else(|| {
                ForgejoError::not_found("comment find-marker", "marker was not found")
            })?;
        Ok(journal.to_comment(&self.http.issue_url(issue_data.id), Some(marker), false))
    }
}
