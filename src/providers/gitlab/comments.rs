use super::{GitlabProvider, model::Note};
use crate::providers::api::{CommentOutput, PhasegentError};
use reqwest::Method;
use serde_json::json;

impl GitlabProvider {
    fn comment_output(
        &self,
        issue: u64,
        note: Note,
        marker: Option<&str>,
        include_body: bool,
        issue_url: Option<&str>,
    ) -> Result<CommentOutput, PhasegentError> {
        if note.id == 0 {
            return Err(PhasegentError::config("invalid GitLab note identity"));
        }
        if note
            .noteable_iid
            .is_some_and(|noteable_iid| noteable_iid != issue)
        {
            return Err(PhasegentError::config(
                "GitLab note issue association mismatch",
            ));
        }
        if note
            .noteable_type
            .as_deref()
            .is_some_and(|kind| !kind.eq_ignore_ascii_case("issue"))
        {
            return Err(PhasegentError::config(
                "GitLab note is not attached to an issue",
            ));
        }
        let html_url = note
            .web_url
            .or_else(|| issue_url.map(|base| format!("{base}#note_{}", note.id)));
        Ok(CommentOutput {
            id: note.id,
            html_url,
            marker: marker.map(str::to_owned),
            body: include_body.then_some(note.body),
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
        let summary = self.load_issue(issue, &self.project()?)?;
        let index = issue.to_string();
        let request = self
            .http
            .request(
                Method::POST,
                &[
                    "projects",
                    self.config.repository(),
                    "issues",
                    &index,
                    "notes",
                ],
            )?
            .json(&json!({"body": body}));
        let note: Note = self.http.write(request, "comment create")?;
        if !note.body.contains(marker) {
            return Err(PhasegentError::config(
                "created comment did not contain marker",
            ));
        }
        self.comment_output(
            issue,
            note,
            Some(marker),
            false,
            summary.html_url.as_deref(),
        )
    }

    pub(super) fn issue_comments(&self, issue: u64) -> Result<Vec<CommentOutput>, PhasegentError> {
        let summary = self.load_issue(issue, &self.project()?)?;
        let index = issue.to_string();
        let notes: Vec<Note> = self.collect(
            &[
                "projects",
                self.config.repository(),
                "issues",
                &index,
                "notes",
            ],
            &[],
            "comment list",
        )?;
        notes
            .into_iter()
            .map(|note| self.comment_output(issue, note, None, true, summary.html_url.as_deref()))
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
