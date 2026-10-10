//! Thin read-only wrappers backing the global all-projects bootstrap.
//!
//! The all-projects preview resolves Redmine roles and project memberships
//! without ever writing, so it needs complete-paginator reads that are not
//! otherwise part of the provider's public surface. Keeping them here avoids
//! widening the general provider API and leaves every write path in the
//! existing membership helpers.

use crate::providers::api::PhasegentError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{RedmineMembership, RedmineRole};

impl RedmineProvider {
    /// Every Redmine role, paginated to completion.
    pub(crate) fn list_roles_readonly(&self) -> Result<Vec<RedmineRole>, PhasegentError> {
        self.http.list_roles()
    }

    /// Every direct membership of `project_id`, paginated to completion.
    pub(crate) fn list_project_memberships(
        &self,
        project_id: u64,
    ) -> Result<Vec<RedmineMembership>, PhasegentError> {
        self.http.list_memberships(project_id)
    }
}
