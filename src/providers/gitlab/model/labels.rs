//! Workflow / tracker label mappings and state helpers.

use crate::providers::api::ForgejoError;

/// Workflow labels that map orchestrator workflow statuses to GitLab
/// project labels. These are managed by this CLI: an issue update
/// removes any prior `workflow::*` labels and applies exactly one of
/// these labels for the requested status. `workflow::closed` is the
/// only label that is paired with GitLab's native close transition.
pub(crate) const WORKFLOW_LABEL_NEW: &str = "workflow::new";
pub(crate) const WORKFLOW_LABEL_IN_PROGRESS: &str = "workflow::in-progress";
pub(crate) const WORKFLOW_LABEL_IN_REVIEW: &str = "workflow::in-review";
pub(crate) const WORKFLOW_LABEL_CHANGES_REQUESTED: &str = "workflow::changes-requested";
pub(crate) const WORKFLOW_LABEL_BLOCKED: &str = "workflow::blocked";
pub(crate) const WORKFLOW_LABEL_RESOLVED: &str = "workflow::resolved";
pub(crate) const WORKFLOW_LABEL_CLOSED: &str = "workflow::closed";
pub(crate) const WORKFLOW_LABEL_CANCELLED: &str = "workflow::cancelled";

/// Every workflow label this CLI recognises, in a stable iteration
/// order. Used by the workflow updater to know which labels are safe
/// to remove from an issue.
pub(crate) const WORKFLOW_LABELS: &[&str] = &[
    WORKFLOW_LABEL_NEW,
    WORKFLOW_LABEL_IN_PROGRESS,
    WORKFLOW_LABEL_IN_REVIEW,
    WORKFLOW_LABEL_CHANGES_REQUESTED,
    WORKFLOW_LABEL_BLOCKED,
    WORKFLOW_LABEL_RESOLVED,
    WORKFLOW_LABEL_CLOSED,
    WORKFLOW_LABEL_CANCELLED,
];

/// Project labels used to encode the Redmine-style Bug / Feature
/// trackers. GitLab has no first-class tracker concept; we use labels
/// so a single create or update request can carry the tracker
/// alongside other fields without a separate API call.
pub(crate) const TRACKER_LABEL_BUG: &str = "type::bug";
pub(crate) const TRACKER_LABEL_FEATURE: &str = "type::feature";

/// Resolve a tracker name to its GitLab label.
///
/// Accepts the case-sensitive names "Bug" and "Feature" (matching the
/// canonical Redmine spelling) and the same names with arbitrary ASCII
/// casing, so a Redmine caller that already learned the
/// `RedmineProvider::select_tracker` rules can pass through. Numeric
/// ids are rejected because the GitLab label convention is name-based
/// and a project can have multiple `type::*` labels; mapping an id to
/// a label would require a separate metadata round trip that Phase 2
/// explicitly defers.
pub(crate) fn tracker_label_from_name(value: &str) -> Result<&'static str, ForgejoError> {
    if value.eq_ignore_ascii_case("Bug") {
        Ok(TRACKER_LABEL_BUG)
    } else if value.eq_ignore_ascii_case("Feature") {
        Ok(TRACKER_LABEL_FEATURE)
    } else {
        Err(ForgejoError::config(format!(
            "GitLab tracker name '{value}' must be Bug or Feature"
        )))
    }
}

/// Resolve a GitLab label back to its canonical tracker name. Used
/// by managed-label validation and future read paths that need the inverse mapping.
pub(crate) fn tracker_name_from_label(label: &str) -> Option<&'static str> {
    match label {
        TRACKER_LABEL_BUG => Some("Bug"),
        TRACKER_LABEL_FEATURE => Some("Feature"),
        _ => None,
    }
}

/// `Err(ForgejoError::config(...))` for unknown statuses so a typo
/// never lands as a silent no-op update.
///
/// Names are case-insensitive so a caller that lower-cases the
/// orchestrator's status value still resolves; the label returned is
/// the canonical lowercase form GitLab receives.
pub(crate) fn workflow_label_from_status(status: &str) -> Result<&'static str, ForgejoError> {
    let normalised = status.trim();
    if normalised.eq_ignore_ascii_case("New") {
        Ok(WORKFLOW_LABEL_NEW)
    } else if normalised.eq_ignore_ascii_case("InProgress")
        || normalised.eq_ignore_ascii_case("In Progress")
    {
        Ok(WORKFLOW_LABEL_IN_PROGRESS)
    } else if normalised.eq_ignore_ascii_case("InReview")
        || normalised.eq_ignore_ascii_case("In Review")
    {
        Ok(WORKFLOW_LABEL_IN_REVIEW)
    } else if normalised.eq_ignore_ascii_case("ChangesRequested")
        || normalised.eq_ignore_ascii_case("Changes Requested")
    {
        Ok(WORKFLOW_LABEL_CHANGES_REQUESTED)
    } else if normalised.eq_ignore_ascii_case("Blocked") {
        Ok(WORKFLOW_LABEL_BLOCKED)
    } else if normalised.eq_ignore_ascii_case("Resolved") {
        Ok(WORKFLOW_LABEL_RESOLVED)
    } else if normalised.eq_ignore_ascii_case("Closed") {
        Ok(WORKFLOW_LABEL_CLOSED)
    } else if normalised.eq_ignore_ascii_case("Cancelled")
        || normalised.eq_ignore_ascii_case("Canceled")
    {
        Ok(WORKFLOW_LABEL_CANCELLED)
    } else {
        Err(ForgejoError::config(format!(
            "GitLab workflow status '{status}' is not recognised; expected \
              New, InProgress, InReview, ChangesRequested, Blocked, Resolved, \
              Closed, or Cancelled"
        )))
    }
}

/// Map a GitLab issue `state` value to the orchestrator's shared
/// `open` / `closed` vocabulary. GitLab only reports `opened` and
/// `closed` at the issue level; the workflow label handles the
/// finer-grained distinction.
pub(crate) fn state_from_gitlab(state: &str) -> &'static str {
    if state.eq_ignore_ascii_case("closed") {
        "closed"
    } else {
        "open"
    }
}

/// Map the orchestrator's `open` / `closed` / `all` state selector to
/// the GitLab issue state filter. `all` is signalled via `None` so the
/// caller knows not to send `state=opened` or `state=closed`.
pub(crate) fn state_query_filter(state: &str) -> Result<Option<&'static str>, ForgejoError> {
    match state {
        "open" => Ok(Some("opened")),
        "closed" => Ok(Some("closed")),
        "all" => Ok(None),
        other => Err(ForgejoError::config(format!(
            "issue state '{other}' must be open, closed, or all"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WORKFLOW_LABELS, state_from_gitlab, state_query_filter, tracker_label_from_name,
        tracker_name_from_label, workflow_label_from_status,
    };
    use crate::providers::api::ForgejoError;

    #[test]
    fn tracker_label_round_trip_for_bug_and_feature() {
        assert_eq!(tracker_label_from_name("Bug").unwrap(), "type::bug");
        assert_eq!(tracker_label_from_name("Feature").unwrap(), "type::feature");
        // Case-insensitive acceptance mirrors the Redmine selector.
        assert_eq!(tracker_label_from_name("bug").unwrap(), "type::bug");
        assert_eq!(tracker_label_from_name("FEATURE").unwrap(), "type::feature");
        assert_eq!(tracker_name_from_label("type::bug"), Some("Bug"));
        assert_eq!(tracker_name_from_label("type::feature"), Some("Feature"));
        assert_eq!(tracker_name_from_label("type::chore"), None);
    }

    #[test]
    fn tracker_label_rejects_other_values() {
        let error = tracker_label_from_name("Task").unwrap_err();
        match error {
            ForgejoError::Config(message) => {
                assert!(message.contains("GitLab tracker name 'Task'"));
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn tracker_label_rejects_numeric_ids() {
        // Numeric ids are explicitly unsupported in Phase 2: the
        // label-based convention is name-only.
        let error = tracker_label_from_name("2").unwrap_err();
        assert!(matches!(error, ForgejoError::Config(_)));
    }

    #[test]
    fn workflow_label_resolves_every_canonical_status() {
        let cases = [
            ("New", "workflow::new"),
            ("InProgress", "workflow::in-progress"),
            ("InProgress ", "workflow::in-progress"),
            ("InProgress\n", "workflow::in-progress"),
            ("inprogress", "workflow::in-progress"),
            ("InReview", "workflow::in-review"),
            ("ChangesRequested", "workflow::changes-requested"),
            ("Blocked", "workflow::blocked"),
            ("Resolved", "workflow::resolved"),
            ("Closed", "workflow::closed"),
            ("Cancelled", "workflow::cancelled"),
            ("Canceled", "workflow::cancelled"),
        ];
        for (input, expected) in cases {
            assert_eq!(workflow_label_from_status(input).unwrap(), expected);
        }
    }

    #[test]
    fn workflow_label_rejects_unknown_status() {
        let error = workflow_label_from_status("Reviewing").unwrap_err();
        match error {
            ForgejoError::Config(message) => assert!(message.contains("not recognised")),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn workflow_labels_list_covers_every_mapping() {
        // The mapping helper must reference exactly the labels that
        // are part of the managed list, otherwise a workflow update
        // would silently leave a stale label attached.
        for label in [
            "workflow::new",
            "workflow::in-progress",
            "workflow::in-review",
            "workflow::changes-requested",
            "workflow::blocked",
            "workflow::resolved",
            "workflow::closed",
            "workflow::cancelled",
        ] {
            assert!(
                WORKFLOW_LABELS.contains(&label),
                "{label} must be in WORKFLOW_LABABS",
            );
        }
        assert_eq!(WORKFLOW_LABELS.len(), 8);
    }

    #[test]
    fn state_query_filter_maps_open_closed_and_all() {
        assert_eq!(state_query_filter("open").unwrap(), Some("opened"));
        assert_eq!(state_query_filter("closed").unwrap(), Some("closed"));
        assert_eq!(state_query_filter("all").unwrap(), None);
        assert!(state_query_filter("bogus").is_err());
    }

    #[test]
    fn state_from_gitlab_uses_shared_open_closed_vocabulary() {
        assert_eq!(state_from_gitlab("opened"), "open");
        assert_eq!(state_from_gitlab("closed"), "closed");
        assert_eq!(state_from_gitlab("OPENED"), "open");
        assert_eq!(state_from_gitlab("Closed"), "closed");
        // Any other value (for example a future GitLab state) falls
        // back to the open bucket rather than silently dropping the
        // issue from search results.
        assert_eq!(state_from_gitlab("locked"), "open");
    }
}
