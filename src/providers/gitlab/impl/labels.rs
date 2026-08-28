//! Labels, tracker labels, workflow status.

use crate::providers::api::{ForgejoError, IssueSummary};
use crate::providers::gitlab::model::{
    ApiLabel, NewLabel, TRACKER_LABEL_BUG, TRACKER_LABEL_FEATURE, UpdateIssue, WORKFLOW_LABELS,
    tracker_label_from_name, tracker_name_from_label, workflow_label_from_status,
};

use super::core::GitlabProvider;
use super::issues::parse_optional_issue;

impl GitlabProvider {
    /// Resolve the issue's current label set, ensuring every managed
    /// workflow and tracker label exists in the project. Returns the
    /// final, fully-attached label list. Used both for read paths
    /// (so the orchestrator can render the issue with up-to-date
    /// labels) and for the workflow update path (so the managed
    /// labels are guaranteed to exist before they are referenced by
    /// the issue update payload).
    pub(crate) fn ensure_labels(&self, labels: &[&str]) -> Result<Vec<String>, ForgejoError> {
        let existing = self.list_project_labels()?;
        let mut ensured = Vec::with_capacity(labels.len());
        for name in labels {
            if existing.iter().any(|candidate| candidate.name == *name) {
                ensured.push((*name).to_owned());
                continue;
            }
            self.create_label(name)?;
            ensured.push((*name).to_owned());
        }
        Ok(ensured)
    }

    pub(crate) fn list_project_labels(&self) -> Result<Vec<ApiLabel>, ForgejoError> {
        let path = self.labels_path();
        self.http.paginate("label list", |http, page| {
            http.get_page::<ApiLabel>(&path, &[("page", page.to_string())], "label list")
        })
    }

    pub(crate) fn create_label(&self, name: &str) -> Result<ApiLabel, ForgejoError> {
        let payload = NewLabel {
            name,
            color: label_color(name),
            description: None,
        };
        self.http
            .post(&self.labels_path(), &payload, "label create")
    }

    /// Apply the orchestrator's `workflow_status` value to an issue:
    /// removes every prior `workflow::*` label, ensures the target
    /// workflow label exists, and pairs `closed` with the native
    /// `state_event=close` (or `state_event=reopen` for every other
    /// target so a previously-closed issue can be re-opened).
    pub(crate) fn set_workflow_status(
        &self,
        iid: u64,
        status: &str,
    ) -> Result<IssueSummary, ForgejoError> {
        let label = workflow_label_from_status(status)?;
        let is_closed = label == "workflow::closed";
        self.apply_status(iid, Some(label), is_closed)
    }

    pub(crate) fn apply_status(
        &self,
        iid: u64,
        label: Option<&str>,
        is_closed: bool,
    ) -> Result<IssueSummary, ForgejoError> {
        // Ensure the target label exists before referencing it.
        if let Some(label) = label {
            self.ensure_labels(&[label])?;
        }
        // Fetch the current issue so we only emit `state_event` when
        // the issue actually needs to transition. GitLab REST v4
        // rejects state_event=reopen on an already-open issue and
        // state_event=close on an already-closed issue with HTTP 400,
        // so emitting the field unconditionally would break the
        // idempotent `status set` path.
        let current = self.get_issue(iid)?;
        let state_event = match (is_closed, current.state.as_str()) {
            (true, "closed") => None,
            (true, _) => Some("close"),
            (false, "closed") => Some("reopen"),
            (false, _) => None,
        };
        // Build the PUT payload: clear every workflow::* label, add
        // the target label, optionally toggle the native state.
        let payload = UpdateIssue {
            description: None,
            state_event,
            add_labels: label
                .map(|value| vec![value.to_owned()])
                .unwrap_or_default(),
            remove_labels: WORKFLOW_LABELS
                .iter()
                .filter(|candidate| label.is_none_or(|target| **candidate != target))
                .map(|value| (*value).to_owned())
                .collect(),
        };
        let response: Option<crate::providers::gitlab::model::ApiIssue> =
            self.http
                .put(&self.issue_path(iid), &payload, "issue status update")?;
        parse_optional_issue(response, "issue status update").map(|issue| issue.into_summary(self))
    }

    /// Resolve a raw `--tracker` value to a GitLab label and ensure
    /// the label exists in the project. Returns the label name ready
    /// for inclusion in a create or update payload.
    pub(crate) fn tracker_label(&self, value: &str) -> Result<String, ForgejoError> {
        let label = tracker_label_from_name(value)?;
        if tracker_name_from_label(label).is_none() {
            return Err(ForgejoError::config(
                "GitLab tracker label mapping is incomplete",
            ));
        }
        self.ensure_labels(&[label])?;
        Ok(label.to_owned())
    }

    /// Resolve `--tracker Bug|Feature` to a label list (one element).
    pub(crate) fn tracker_label_list(&self, value: &str) -> Result<Vec<String>, ForgejoError> {
        Ok(vec![self.tracker_label(value)?])
    }

    /// Map a managed tracker label to its opposite managed label, or
    /// `None` for any other label. Used by
    /// [`crate::providers::gitlab::r#impl::issues::GitlabProvider::update_body_with_labels`] so
    /// switching from one tracker to the other does not leave both
    /// `type::bug` and `type::feature` attached to the same issue.
    pub(crate) fn opposite_tracker_label(label: &str) -> Option<&'static str> {
        match label {
            TRACKER_LABEL_BUG => Some(TRACKER_LABEL_FEATURE),
            TRACKER_LABEL_FEATURE => Some(TRACKER_LABEL_BUG),
            _ => None,
        }
    }
}

pub(crate) fn label_color(name: &str) -> &'static str {
    // GitLab requires a valid hex color for new labels. The exact
    // shade does not matter for the orchestrator workflow, so each
    // managed label gets a stable, distinguishable color that does
    // not collide with the GitLab default palette.
    match name {
        "workflow::new" => "#1f75cb",
        "workflow::in-progress" => "#e6a23c",
        "workflow::in-review" => "#8e44ad",
        "workflow::changes-requested" => "#d63a3a",
        "workflow::blocked" => "#6c757d",
        "workflow::resolved" => "#28a745",
        "workflow::closed" => "#222222",
        "workflow::cancelled" => "#bf2e2e",
        TRACKER_LABEL_BUG => "#d63a3a",
        TRACKER_LABEL_FEATURE => "#28a745",
        other => {
            // Stable hash-based fallback for any future label that
            // does not get an explicit assignment above.
            let bytes = other.as_bytes();
            let hash = bytes.iter().fold(0u32, |acc, byte| {
                (acc.wrapping_mul(31)).wrapping_add(*byte as u32)
            });
            let r = (hash & 0xff) as u8;
            let g = ((hash >> 8) & 0xff) as u8;
            let b = ((hash >> 16) & 0xff) as u8;
            let hex = format!("#{r:02x}{g:02x}{b:02x}");
            Box::leak(hex.into_boxed_str())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::label_color;

    #[test]
    fn label_color_is_a_valid_hex_string_for_every_managed_label() {
        let labels = [
            "workflow::new",
            "workflow::in-progress",
            "workflow::in-review",
            "workflow::changes-requested",
            "workflow::blocked",
            "workflow::resolved",
            "workflow::closed",
            "workflow::cancelled",
            "type::bug",
            "type::feature",
        ];
        for label in labels {
            let color = label_color(label);
            assert!(
                color.starts_with('#') && color.len() == 7,
                "{label} produced invalid color {color}"
            );
            assert!(
                color[1..].chars().all(|c| c.is_ascii_hexdigit()),
                "{label} produced non-hex color {color}"
            );
        }
    }

    #[test]
    fn label_color_fallback_is_deterministic_for_unknown_names() {
        let first = label_color("custom::thing");
        let second = label_color("custom::thing");
        assert_eq!(first, second);
    }
}
