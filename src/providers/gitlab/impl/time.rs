//! Spent-time / time-estimate projection.

use crate::providers::api::ForgejoError;
use crate::providers::gitlab::model::{
    ApiSpentTimeSummary, NewSpentTime, NewTimeEstimate, format_gitlab_duration,
};

use super::core::GitlabProvider;

impl GitlabProvider {
    /// `POST /projects/:id/issues/:iid/add_spent_time` with a GitLab
    /// human-format duration and an optional run-marker summary. The
    /// duration is validated locally so a non-positive value never
    /// reaches the wire. The endpoint returns a summary object; the
    /// `ApiSpentTimeSummary` decoder keeps the running totals so the
    /// caller can render the updated spent time without a follow-up
    /// GET.
    pub(crate) fn add_spent_time(
        &self,
        iid: u64,
        duration_seconds: i64,
        summary: Option<&str>,
    ) -> Result<ApiSpentTimeSummary, ForgejoError> {
        if iid == 0 {
            return Err(ForgejoError::config(
                "GitLab issue iid must be greater than zero",
            ));
        }
        if duration_seconds <= 0 {
            return Err(ForgejoError::config("GitLab spent time must be positive"));
        }
        let duration = format_gitlab_duration(duration_seconds);
        let payload = NewSpentTime {
            duration: &duration,
            summary,
        };
        self.http
            .post(&self.spent_time_path(iid), &payload, "time spent create")
    }

    /// `POST /projects/:id/issues/:iid/time_estimate` with a GitLab
    /// human-format duration. Mirrors the spent time contract: the
    /// duration is validated locally before it reaches the wire so a
    /// non-positive value surfaces as a structured config error.
    pub(crate) fn set_time_estimate(
        &self,
        iid: u64,
        duration_seconds: i64,
    ) -> Result<ApiSpentTimeSummary, ForgejoError> {
        if iid == 0 {
            return Err(ForgejoError::config(
                "GitLab issue iid must be greater than zero",
            ));
        }
        if duration_seconds <= 0 {
            return Err(ForgejoError::config(
                "GitLab time estimate must be positive",
            ));
        }
        let duration = format_gitlab_duration(duration_seconds);
        let payload = NewTimeEstimate {
            duration: &duration,
        };
        self.http
            .post(&self.time_estimate_path(iid), &payload, "time estimate set")
    }
}
