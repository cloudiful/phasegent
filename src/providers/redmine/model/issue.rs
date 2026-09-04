use crate::providers::api::{CommentOutput, IssueSummary};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineIssueResponse {
    pub(crate) issue: RedmineIssue,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineIssueCollection {
    #[serde(default)]
    pub(crate) issues: Vec<RedmineIssue>,
    pub(crate) total_count: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineIssue {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) subject: String,
    #[serde(default)]
    pub(crate) description: String,
    pub(crate) status: Option<RedmineStatus>,
    #[serde(default)]
    pub(crate) journals: Vec<RedmineJournal>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineStatus {
    /// Server-assigned status id. Missing or zero values indicate a
    /// legacy response shape (older Redmine releases, mock fixtures)
    /// where the caller cannot verify a status transition by id and must
    /// rely on the `is_closed` flag or a follow-up `GET` instead.
    #[serde(default)]
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) name: String,
    pub(crate) is_closed: Option<bool>,
}

impl RedmineStatus {
    /// `Some(id)` when the response carries a usable status id, `None`
    /// otherwise. The provider-level status verification treats the
    /// `None` case as "cannot verify by id" and falls back to either
    /// `is_closed` (for close) or a follow-up `GET`.
    pub(crate) fn known_id(&self) -> Option<u64> {
        if self.id > 0 { Some(self.id) } else { None }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineJournal {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) notes: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineErrorResponse {
    #[serde(default)]
    pub(crate) errors: Vec<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewIssue<'a> {
    pub(crate) issue: RedmineNewIssueFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewIssueFields<'a> {
    pub(crate) project_id: Value,
    pub(crate) subject: &'a str,
    pub(crate) description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tracker_id: Option<u64>,
    // Native Redmine planning fields. All are omitted when unset so the
    // legacy create payload stays byte-identical for existing callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_issue_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fixed_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) due_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) estimated_hours: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) done_ratio: Option<u64>,
}

/// Native Redmine issue planning fields shared by create and update
/// payloads. Every field is optional and omitted from JSON when unset so
/// callers that do not use planning flags keep the exact legacy payload.
#[derive(Clone, Debug, Default)]
pub(crate) struct IssuePlanning {
    pub(crate) parent_issue_id: Option<u64>,
    pub(crate) fixed_version_id: Option<u64>,
    pub(crate) start_date: Option<String>,
    pub(crate) due_date: Option<String>,
    pub(crate) estimated_hours: Option<f64>,
    pub(crate) done_ratio: Option<u64>,
}

impl IssuePlanning {
    /// True when no planning field is set; callers can keep using the
    /// plain provider paths that never grew planning fields.
    pub(crate) fn is_empty(&self) -> bool {
        self.parent_issue_id.is_none()
            && self.fixed_version_id.is_none()
            && self.start_date.is_none()
            && self.due_date.is_none()
            && self.estimated_hours.is_none()
            && self.done_ratio.is_none()
    }
}

impl<'a> RedmineNewIssue<'a> {
    pub(crate) fn new(project_id: &'a str, subject: &'a str, description: &'a str) -> Self {
        let project_id = project_id
            .parse::<u64>()
            .map_or_else(|_| Value::String(project_id.to_owned()), Value::from);
        Self {
            issue: RedmineNewIssueFields {
                project_id,
                subject,
                description,
                tracker_id: None,
                parent_issue_id: None,
                fixed_version_id: None,
                start_date: None,
                due_date: None,
                estimated_hours: None,
                done_ratio: None,
            },
        }
    }

    /// Attach an explicit tracker (for example Bug or Feature) resolved
    /// through `/trackers.json`; omitted for callers that do not select one.
    pub(crate) fn with_tracker(mut self, tracker_id: u64) -> Self {
        self.issue.tracker_id = Some(tracker_id);
        self
    }

    pub(crate) fn with_tracker_option(self, tracker_id: Option<u64>) -> Self {
        match tracker_id {
            Some(tracker_id) => self.with_tracker(tracker_id),
            None => self,
        }
    }

    /// Attach native planning fields (parent, version, dates, estimates).
    /// Fields absent from `planning` stay out of the serialized payload.
    pub(crate) fn with_planning(mut self, planning: &IssuePlanning) -> Self {
        let target = &mut self.issue;
        target.parent_issue_id = planning.parent_issue_id;
        target.fixed_version_id = planning.fixed_version_id;
        target.start_date = planning.start_date.clone();
        target.due_date = planning.due_date.clone();
        target.estimated_hours = planning.estimated_hours;
        target.done_ratio = planning.done_ratio;
        self
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineUpdateIssue<'a> {
    pub(crate) issue: RedmineUpdateIssueFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineUpdateIssueFields<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tracker_id: Option<u64>,
    // Native planning fields, omitted when unset so the legacy update
    // payload stays byte-identical for existing callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_issue_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fixed_version_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) due_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) estimated_hours: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) done_ratio: Option<u64>,
}

impl<'a> RedmineUpdateIssueFields<'a> {
    fn empty(
        description: Option<&'a str>,
        status_id: Option<u64>,
        tracker_id: Option<u64>,
    ) -> Self {
        Self {
            description,
            status_id,
            tracker_id,
            parent_issue_id: None,
            fixed_version_id: None,
            start_date: None,
            due_date: None,
            estimated_hours: None,
            done_ratio: None,
        }
    }
}

impl<'a> RedmineUpdateIssue<'a> {
    pub(crate) fn description(description: &'a str) -> Self {
        Self {
            issue: RedmineUpdateIssueFields::empty(Some(description), None, None),
        }
    }

    /// Update the body and optionally re-target the tracker in one PUT so
    /// `issue update-body --tracker ...` stays a single atomic request.
    pub(crate) fn description_with_tracker(description: &'a str, tracker_id: u64) -> Self {
        Self {
            issue: RedmineUpdateIssueFields::empty(Some(description), None, Some(tracker_id)),
        }
    }

    pub(crate) fn status(status_id: u64) -> Self {
        Self {
            issue: RedmineUpdateIssueFields::empty(None, Some(status_id), None),
        }
    }

    /// Attach native planning fields (parent, version, dates, estimates).
    /// Fields absent from `planning` stay out of the serialized payload.
    pub(crate) fn with_planning(mut self, planning: &IssuePlanning) -> Self {
        self.issue.parent_issue_id = planning.parent_issue_id;
        self.issue.fixed_version_id = planning.fixed_version_id;
        self.issue.start_date = planning.start_date.clone();
        self.issue.due_date = planning.due_date.clone();
        self.issue.estimated_hours = planning.estimated_hours;
        self.issue.done_ratio = planning.done_ratio;
        self
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNotes<'a> {
    pub(crate) issue: RedmineNotesFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNotesFields<'a> {
    pub(crate) notes: &'a str,
}

impl RedmineIssue {
    pub(crate) fn into_summary(self, html_url: String) -> IssueSummary {
        let state = self.state();
        IssueSummary {
            id: self.id,
            number: self.id,
            title: self.subject,
            body: self.description,
            state,
            html_url: Some(html_url),
        }
    }

    pub(crate) fn matches_state(&self, state: &str) -> bool {
        match state {
            "all" => true,
            "open" => !self.status_is_closed(),
            "closed" => self.status_is_closed(),
            _ => false,
        }
    }

    pub(crate) fn state(&self) -> String {
        self.status
            .as_ref()
            .map(|status| {
                status.is_closed.map_or_else(
                    || status.name.clone(),
                    |closed| {
                        if closed {
                            "closed".to_owned()
                        } else {
                            "open".to_owned()
                        }
                    },
                )
            })
            .unwrap_or_else(|| "unknown".to_owned())
    }

    pub(crate) fn find_journal(&self, body: &str, marker: &str) -> Option<&RedmineJournal> {
        self.journals
            .iter()
            .rev()
            .find(|journal| journal.notes == body || journal.notes.contains(marker))
    }

    fn status_is_closed(&self) -> bool {
        self.status.as_ref().is_some_and(|status| {
            status
                .is_closed
                .unwrap_or_else(|| status.name.to_ascii_lowercase().contains("closed"))
        })
    }
}

impl RedmineJournal {
    /// Render a journal as a comment whose URL anchors the exact note so
    /// audit references land on `#note-<id>` instead of the issue top.
    pub(crate) fn to_comment(
        &self,
        issue_url: &str,
        marker: Option<&str>,
        include_body: bool,
    ) -> CommentOutput {
        CommentOutput {
            id: self.id,
            html_url: Some(format!("{issue_url}#note-{}", self.id)),
            marker: marker
                .map(str::to_owned)
                .or_else(|| marker_from_notes(&self.notes)),
            body: include_body.then(|| self.notes.clone()),
        }
    }
}

fn marker_from_notes(notes: &str) -> Option<String> {
    let start = notes.find("<!--")?;
    let end = notes[start..].find("-->")? + start + 3;
    Some(notes[start..end].to_owned())
}

/// Redmine issue attachment upload protocol: the raw bytes are posted to
/// `/uploads.json?filename=<basename>` with `application/octet-stream`,
/// producing a transient token that is then referenced in a
/// `PUT /issues/<id>.json` with `{"issue":{"uploads":[...]}}`.
#[derive(Debug, Serialize)]
pub(crate) struct RedmineIssueUploadUpdate<'a> {
    pub(crate) issue: RedmineIssueUploadFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineIssueUploadFields<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) notes: Option<&'a str>,
    pub(crate) uploads: Vec<RedmineUploadEntry<'a>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineUploadEntry<'a> {
    pub(crate) token: &'a str,
    pub(crate) filename: &'a str,
}

/// Compact JSON returned by the CLI after a successful attachment upload.
/// The transient upload token is never exposed.
#[derive(Debug, Serialize)]
pub struct AttachmentUploadOutput {
    pub issue: u64,
    pub filename: String,
    pub bytes: usize,
    pub success: bool,
}
