//! GitLab note CRUD, find-marker and comment conversions.

use crate::providers::api::{CommentOutput, ForgejoError};
use crate::providers::gitlab::model::{ApiNote, NewNote};

use super::core::GitlabProvider;

impl GitlabProvider {
    /// `POST /projects/:id/issues/:iid/notes` with the caller's body.
    /// GitLab note ids and URLs are stable, so the returned
    /// `CommentOutput` carries both.
    pub(crate) fn create_note(&self, iid: u64, body: &str) -> Result<CommentOutput, ForgejoError> {
        let payload = NewNote { body };
        let note: ApiNote = self
            .http
            .post(&self.notes_path(iid), &payload, "comment create")?;
        // GitLab renders notes inline on the parent issue page, so
        // the canonical note URL is `<issue_web_url>#note_<id>`.
        // Fetch the issue to obtain its `web_url`; if the API omits
        // it, fall back to `None` rather than synthesising an API
        // path that is not browsable.
        let issue_web_url = self.get_issue(iid)?.html_url;
        Ok(note.into_output(issue_web_url.as_deref()))
    }

    /// `GET /projects/:id/issues/:iid/notes/:note_id` for one note.
    pub(crate) fn get_note(&self, iid: u64, note_id: u64) -> Result<CommentOutput, ForgejoError> {
        let note: ApiNote = self
            .http
            .get(&self.note_path(iid, note_id), &[], "comment get")?;
        let issue_web_url = self.get_issue(iid)?.html_url;
        Ok(note.into_output(issue_web_url.as_deref()))
    }

    /// `GET /projects/:id/issues/:iid/notes` paginated until
    /// completion. System notes (`system: true`) are still walked,
    /// but only matched against the marker if the body contains it,
    /// which is extremely unlikely for system events. The marker
    /// lookup therefore never accidentally returns a system note as
    /// an audit note unless the operator deliberately embedded the
    /// marker in such a note.
    pub(crate) fn find_marker(
        &self,
        iid: u64,
        marker: &str,
    ) -> Result<CommentOutput, ForgejoError> {
        if marker.is_empty() {
            return Err(ForgejoError::config("marker cannot be empty"));
        }
        // Resolve the parent issue once so every returned note can
        // carry a browsable `<issue_web_url>#note_<id>` URL.
        let issue_web_url = self.get_issue(iid)?.html_url;
        let path = self.notes_path(iid);
        let notes = self.http.paginate("comment list", |http, page| {
            http.get_page::<ApiNote>(&path, &[("page", page.to_string())], "comment list")
        })?;
        notes
            .into_iter()
            .find(|note| note.body.contains(marker))
            .map(|note| {
                note.into_output(issue_web_url.as_deref())
                    .with_marker(marker.to_owned())
            })
            .ok_or_else(|| ForgejoError::not_found("comment find-marker", "marker was not found"))
    }
}

impl ApiNote {
    /// Build a [`CommentOutput`] for the deserialised GitLab note.
    ///
    /// `issue_web_url` is the parent issue's `web_url` returned by
    /// GitLab (for example `https://gitlab.example/group/project/-/issues/7`).
    /// GitLab renders notes inline on the issue page, so the canonical
    /// note URL is `<issue_web_url>#note_<id>`. When the API did not
    /// surface a `web_url` for the parent issue, the function returns
    /// `None` rather than synthesising an `/api/v4` path that is not
    /// browsable from a web browser.
    pub(crate) fn into_output(self, issue_web_url: Option<&str>) -> CommentOutput {
        let html_url =
            issue_web_url.map(|base| format!("{}#note_{}", base.trim_end_matches('/'), self.id));
        CommentOutput {
            id: self.id,
            html_url,
            marker: None,
            body: Some(self.body.clone()),
        }
    }
}

impl CommentOutput {
    /// Replace the inferred marker with an explicit caller-provided
    /// marker. Used by `find_marker` so the returned `CommentOutput`
    /// always carries the marker that was searched for.
    pub(crate) fn with_marker(mut self, marker: String) -> Self {
        self.marker = Some(marker);
        self
    }
}
