//! Static GitLab workflow status catalogue.
//!
//! GitLab has no native issue status enum; the orchestrator instead
//! drives a workflow by attaching exactly one `workflow::*` label to
//! each issue and pairing `closed` with the native `state_event=close`
//! transition. Phase 2 surfaces the same canonical workflow that
//! `RedmineProvider::list_issue_statuses` does on the Redmine side, so
//! the shared CLI `status list` (and downstream `status next` planning
//! surfaces) see the same catalogue no matter which provider resolved.
//!
//! The catalogue is read-only: every entry mirrors a label that
//! `src/providers/gitlab/model/labels.rs` already manages, and the
//! stable ids (1..=8) are the local ordering used by the canonical
//! phase workflow policy. They never collide with Redmine status ids
//! because they only ever surface through the GitLab provider arm; the
//! `status next` policy still resolves names case-insensitively, so
//! the static ids are advisory and not persisted anywhere.

use crate::providers::api::ForgejoError;
use crate::providers::redmine::model::RedmineIssueStatus;

use super::core::GitlabProvider;

/// Stable identifier for the canonical workflow catalogue. It is
/// surfaced via the existing `RedmineIssueStatus` shape so the shared
/// CLI output stays identical across providers. The string is the
/// single source of truth that an operator can grep for when
/// differentiating GitLab workflow labels from Redmine status ids in
/// audit comments.
///
/// `#[allow(dead_code)]` keeps the constant available for future
/// audit comments and orchestrator prompts without forcing the
/// current Phase 2 surfaces to consume it; the value stays the
/// single source of truth and a future call site can adopt it.
#[allow(dead_code)]
pub(crate) const WORKFLOW_CATALOGUE_SOURCE: &str = "phasegent/gitlab-workflow-labels@v1";

/// Caveat appended to every catalogue entry so an operator knows the
/// static id is an advisory mapping, not a server-side primary key.
/// Mirrors `RedmineProvider::STATUS_POLICY_CAVEAT` so the audit
/// vocabulary stays symmetric.
///
/// `#[allow(dead_code)]` keeps the caveat available for future audit
/// comments and orchestrator prompts.
#[allow(dead_code)]
pub(crate) const WORKFLOW_CATALOGUE_CAVEAT: &str = "Static mapping: the GitLab server does not expose a native status enum; \
     the orchestrator drives the workflow via workflow::* labels paired with \
     state_event when needed; the canonical ids below are advisory and may \
     differ from Redmine status ids.";

impl GitlabProvider {
    /// Return the static GitLab workflow catalogue as the shared
    /// `RedmineIssueStatus` shape. No HTTP traffic: GitLab has no
    /// native status enum, so the workflow is encoded as labels and
    /// the orchestrator surfaces the catalogue locally. The order is
    /// the canonical phase workflow order (New → In Progress → In
    /// Review → Changes Requested → Blocked → Resolved → Closed →
    /// Cancelled); callers that need a stable iteration order rely
    /// on this so a `status list` printout is reproducible across
    /// runs.
    pub(crate) fn list_workflow_statuses(&self) -> Result<Vec<RedmineIssueStatus>, ForgejoError> {
        Ok(static_workflow_statuses())
    }
}

/// Build the canonical workflow status catalogue. Public to the
/// module so contract tests and the dispatcher layer can render the
/// same entries without instantiating a provider. The function is the
/// single source of truth; changing the order or the closed flag
/// here flows through every caller.
fn static_workflow_statuses() -> Vec<RedmineIssueStatus> {
    vec![
        RedmineIssueStatus {
            id: 1,
            name: "New".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 2,
            name: "In Progress".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 3,
            name: "In Review".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 4,
            name: "Changes Requested".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 5,
            name: "Blocked".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 6,
            name: "Resolved".to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: 7,
            name: "Closed".to_owned(),
            is_closed: true,
        },
        RedmineIssueStatus {
            id: 8,
            name: "Cancelled".to_owned(),
            is_closed: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_covers_every_workflow_label_in_canonical_order() {
        let statuses = static_workflow_statuses();
        assert_eq!(statuses.len(), 8);
        let names: Vec<&str> = statuses.iter().map(|status| status.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "New",
                "In Progress",
                "In Review",
                "Changes Requested",
                "Blocked",
                "Resolved",
                "Closed",
                "Cancelled",
            ]
        );
        // Closed and Cancelled are the only closed statuses in the
        // canonical workflow; every other entry stays open so a
        // `status next` policy lookup sees a consistent open/closed
        // split with the Redmine catalogue.
        assert!(!statuses[0].is_closed);
        assert!(!statuses[5].is_closed);
        assert!(statuses[6].is_closed);
        assert!(statuses[7].is_closed);
        // Stable ids: 1..=8, so the audit comment can rely on the
        // ordering without re-decoding the catalogue every run.
        let ids: Vec<u64> = statuses.iter().map(|status| status.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn catalogue_labels_match_managed_workflow_labels() {
        // Every entry's canonical name must round-trip through the
        // managed label mapping in `model::labels`; otherwise a
        // `status set` flow that resolves the catalogue name would
        // fail to find the corresponding label.
        use crate::providers::gitlab::model::{
            WORKFLOW_LABEL_BLOCKED, WORKFLOW_LABEL_CANCELLED, WORKFLOW_LABEL_CHANGES_REQUESTED,
            WORKFLOW_LABEL_CLOSED, WORKFLOW_LABEL_IN_PROGRESS, WORKFLOW_LABEL_IN_REVIEW,
            WORKFLOW_LABEL_NEW, WORKFLOW_LABEL_RESOLVED,
        };
        let statuses = static_workflow_statuses();
        for status in &statuses {
            let label = crate::providers::gitlab::model::workflow_label_from_status(&status.name)
                .unwrap_or_else(|error| {
                    panic!(
                        "catalogue status {:?} did not resolve: {error}",
                        status.name
                    )
                });
            assert!(
                [
                    WORKFLOW_LABEL_NEW,
                    WORKFLOW_LABEL_IN_PROGRESS,
                    WORKFLOW_LABEL_IN_REVIEW,
                    WORKFLOW_LABEL_CHANGES_REQUESTED,
                    WORKFLOW_LABEL_BLOCKED,
                    WORKFLOW_LABEL_RESOLVED,
                    WORKFLOW_LABEL_CLOSED,
                    WORKFLOW_LABEL_CANCELLED,
                ]
                .contains(&label),
                "catalogue status {:?} resolved to unexpected label {label}",
                status.name,
            );
        }
    }

    #[test]
    fn provider_list_workflow_statuses_returns_static_catalogue() {
        let provider = GitlabProvider::new(
            crate::providers::config::GitlabConfig::new("https://gitlab.example/api/v4", 42),
            "test-token".to_owned(),
        )
        .unwrap();
        let statuses = provider.list_workflow_statuses().unwrap();
        assert_eq!(statuses.len(), 8);
        assert_eq!(statuses[6].name, "Closed");
        assert!(statuses[6].is_closed);
    }
}
