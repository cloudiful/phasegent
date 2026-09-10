//! GitLab milestone → Redmine-style version list (`GET /projects/:id/milestones`).
//!
//! GitLab has no native `versions` concept; project versions are stored
//! as milestones. Phase 2 exposes the equivalent read by mapping the
//! milestone payload to the shared `RedmineVersion` shape so the
//! existing `version list` CLI command (and any downstream planning
//! flow that consumes `RedmineVersion`) works against GitLab without a
//! separate code path.
//!
//! The mapping is intentionally conservative:
//!
//! * `id` mirrors `RedmineVersion::id` and is required because the
//!   orchestrator uses it as the planning-field primary key.
//! * `title` becomes `name` so the CLI output stays human-readable.
//! * `state` (`active` | `closed` | `upcoming`) becomes the
//!   `status` string verbatim so a downstream consumer can branch on
//!   it without re-decoding GitLab semantics.
//! * `due_date` is forwarded as-is (ISO-8601) or `None` when the
//!   milestone is undated.
//!
//! No fields are written back: the read path is the only Phase 2
//! contract; future write automation lives in a later phase.

use crate::providers::api::ForgejoError;
use crate::providers::gitlab::model::ApiMilestone;
use crate::providers::redmine::model::RedmineVersion;

use super::core::GitlabProvider;

impl GitlabProvider {
    /// `GET /projects/:id/milestones` paginated across all pages.
    ///
    /// GitLab returns the milestone list as a top-level JSON array
    /// (no wrapper); the shared `paginate` helper walks every page,
    /// repeats the `x-next-page`/`x-total-pages` heuristics, and stops
    /// before the safety cap. Each page is mapped onto the shared
    /// `RedmineVersion` shape so the existing `version list` CLI
    /// command (and future planning flows) work against GitLab
    /// without a separate code path.
    pub(crate) fn list_milestones(&self) -> Result<Vec<RedmineVersion>, ForgejoError> {
        let path = self.milestones_path();
        let milestones: Vec<ApiMilestone> =
            self.http.paginate("milestone list", |http, page| {
                http.get_page::<ApiMilestone>(
                    &path,
                    &[("page", page.to_string())],
                    "milestone list",
                )
            })?;
        Ok(milestones.into_iter().map(Into::into).collect())
    }
}

impl From<ApiMilestone> for RedmineVersion {
    fn from(milestone: ApiMilestone) -> Self {
        // GitLab reports `active`, `closed`, or `upcoming`; we
        // forward the raw value so a downstream consumer can branch
        // on the original GitLab semantics. Empty / unknown states
        // collapse to an empty string so the Redmine-shape view stays
        // consistent with how a Redmine response with no status
        // would render.
        let status = milestone
            .state
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("")
            .to_owned();
        RedmineVersion {
            id: milestone.id,
            // GitLab's `title` is the human label; `iid` is the
            // project-scoped milestone number which has no Redmine
            // equivalent. Falls back to a derived `Milestone <id>`
            // placeholder when the API omitted `title` so the CLI
            // output is never empty.
            name: milestone
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Milestone {}", milestone.id)),
            status,
            due_date: milestone.due_date,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn milestone_maps_to_redmine_version_with_status_and_due_date() {
        let milestone = ApiMilestone {
            id: 12,
            iid: Some(1),
            title: Some("Sprint 1".to_owned()),
            description: Some("first sprint".to_owned()),
            state: Some("active".to_owned()),
            due_date: Some("2026-09-30".to_owned()),
            start_date: Some("2026-09-01".to_owned()),
            web_url: Some("https://gitlab.example/group/project/-/milestones/1".to_owned()),
        };
        let version: RedmineVersion = milestone.into();
        assert_eq!(version.id, 12);
        assert_eq!(version.name, "Sprint 1");
        assert_eq!(version.status, "active");
        assert_eq!(version.due_date.as_deref(), Some("2026-09-30"));
    }

    #[test]
    fn milestone_maps_closed_state_and_omits_due_date() {
        let milestone = ApiMilestone {
            id: 14,
            iid: None,
            title: Some("Backlog".to_owned()),
            description: None,
            state: Some("closed".to_owned()),
            due_date: None,
            start_date: None,
            web_url: None,
        };
        let version: RedmineVersion = milestone.into();
        assert_eq!(version.id, 14);
        assert_eq!(version.name, "Backlog");
        assert_eq!(version.status, "closed");
        assert!(version.due_date.is_none());
    }

    #[test]
    fn milestone_falls_back_to_default_name_when_title_missing() {
        let milestone = ApiMilestone {
            id: 99,
            iid: None,
            title: None,
            description: None,
            state: None,
            due_date: None,
            start_date: None,
            web_url: None,
        };
        let version: RedmineVersion = milestone.into();
        assert_eq!(version.id, 99);
        assert_eq!(version.name, "Milestone 99");
        assert_eq!(version.status, "");
        assert!(version.due_date.is_none());
    }

    #[test]
    fn milestone_trims_whitespace_status_and_omits_blank_state() {
        let milestone = ApiMilestone {
            id: 100,
            iid: None,
            title: Some("   ".to_owned()),
            description: None,
            state: Some("  active  ".to_owned()),
            due_date: None,
            start_date: None,
            web_url: None,
        };
        let version: RedmineVersion = milestone.into();
        // Blank title falls back to the id-based placeholder; the
        // status string is trimmed but otherwise preserved.
        assert_eq!(version.name, "Milestone 100");
        assert_eq!(version.status, "active");
    }
}
