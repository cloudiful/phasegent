use super::{ForgejoProvider, model::Comment};
use crate::providers::api::{CommentOutput, PhasegentError};
use reqwest::Method;
use serde_json::json;

impl ForgejoProvider {
    fn comment_output(
        &self,
        issue: u64,
        comment: Comment,
        marker: Option<&str>,
        include_body: bool,
    ) -> Result<CommentOutput, PhasegentError> {
        let index = issue.to_string();
        let expected = self
            .http
            .request(Method::GET, &["issues", &index])?
            .build()
            .map_err(|_| PhasegentError::config("invalid Forgejo issue URL"))?;
        let actual = url::Url::parse(&comment.issue_url)
            .map_err(|_| PhasegentError::config("invalid Forgejo comment issue association"))?;
        if &actual != expected.url() || comment.id == 0 {
            return Err(PhasegentError::config(
                "Forgejo comment issue association mismatch",
            ));
        }
        Ok(CommentOutput {
            id: comment.id,
            html_url: comment.html_url,
            marker: marker.map(str::to_owned),
            body: include_body.then_some(comment.body),
        })
    }

    pub(super) fn create_issue_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, PhasegentError> {
        if marker.is_empty() || !body.contains(marker) {
            return Err(PhasegentError::config(
                "comment body must contain a nonempty marker",
            ));
        }
        self.load_issue(issue, &self.repository()?)?;
        let index = issue.to_string();
        let request = self
            .http
            .request(Method::POST, &["issues", &index, "comments"])?
            .json(&json!({"body": body}));
        let comment: Comment = self.http.write(request, "comment create")?;
        if !comment.body.contains(marker) {
            return Err(PhasegentError::config(
                "created comment did not contain marker",
            ));
        }
        self.comment_output(issue, comment, Some(marker), false)
    }

    pub(super) fn issue_comments(&self, issue: u64) -> Result<Vec<CommentOutput>, PhasegentError> {
        self.load_issue(issue, &self.repository()?)?;
        let index = issue.to_string();
        let comments: Vec<Comment> =
            self.collect(&["issues", &index, "comments"], &[], "comment list")?;
        comments
            .into_iter()
            .map(|comment| self.comment_output(issue, comment, None, true))
            .collect()
    }

    pub(super) fn get_issue_comment(
        &self,
        issue: u64,
        comment: u64,
    ) -> Result<CommentOutput, PhasegentError> {
        self.issue_comments(issue)?
            .into_iter()
            .find(|item| item.id == comment)
            .ok_or_else(|| {
                PhasegentError::not_found(
                    "comment get",
                    "comment was not found in the specified issue",
                )
            })
    }

    pub(super) fn find_issue_marker(
        &self,
        issue: u64,
        marker: &str,
    ) -> Result<CommentOutput, PhasegentError> {
        if marker.is_empty() {
            return Err(PhasegentError::config("marker cannot be empty"));
        }
        let mut comment = self
            .issue_comments(issue)?
            .into_iter()
            .find(|item| {
                item.body
                    .as_deref()
                    .is_some_and(|body| body.contains(marker))
            })
            .ok_or_else(|| {
                PhasegentError::not_found("comment find-marker", "marker was not found")
            })?;
        comment.marker = Some(marker.to_owned());
        comment.body = None;
        Ok(comment)
    }
}
