//! Local status catalogue + transition policy (issue 211 P2).

use super::model::local_statuses;
use super::LocalProvider;
use crate::providers::api::ForgejoError;
use crate::providers::RedmineIssueStatus;

/// Allowed next statuses for a canonical status, derived from
/// `infra::local_schema::STATUS_TRANSITION_SEED` so the local
/// workflow and the P4 seeds share one literal.
pub(crate) fn allowed_next_for(status: &str) -> Vec<&'static str> {
    crate::infra::local_schema::STATUS_TRANSITION_SEED
        .iter()
        .filter(|(from, _)| *from == status)
        .map(|(_, to)| *to)
        .collect()
}

/// True for same-status no-ops and seeded edges; false otherwise.
pub(crate) fn is_transition_allowed(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    crate::infra::local_schema::STATUS_TRANSITION_SEED
        .iter()
        .any(|(seed_from, seed_to)| *seed_from == from && *seed_to == to)
}

impl LocalProvider {
    pub fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, ForgejoError> {
        Ok(local_statuses())
    }
}
