//! Serializable global all-projects bootstrap report.
//!
//! These types are the single source of truth for the global JSON contract;
//! the CLI serializes them directly and the exit code derives from the same
//! report, so the printed payload and the process outcome can never disagree.

use serde::Serialize;

pub(crate) const MODE_ALL_PROJECTS: &str = "all-projects";

pub(crate) const STATUS_EXISTING: &str = "existing";
pub(crate) const STATUS_PLANNED: &str = "planned";
pub(crate) const STATUS_PROVISIONED: &str = "provisioned";
pub(crate) const STATUS_ADDED: &str = "added";
pub(crate) const STATUS_UPDATED: &str = "updated";
pub(crate) const STATUS_WARNING: &str = "warning";
pub(crate) const STATUS_BLOCKED: &str = "blocked";

/// Whether a membership status is actionable (does not block readiness).
pub(crate) fn membership_actionable(status: &str) -> bool {
    matches!(
        status,
        STATUS_ADDED | STATUS_UPDATED | STATUS_EXISTING | STATUS_PLANNED
    )
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct RoleIdentityReport {
    pub(crate) role: String,
    pub(crate) login: String,
    pub(crate) user_id: Option<u64>,
    pub(crate) role_name: String,
    pub(crate) status: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct MembershipReport {
    pub(crate) role: String,
    pub(crate) user_id: Option<u64>,
    pub(crate) login: String,
    pub(crate) role_name: String,
    pub(crate) status: String,
    pub(crate) warning: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct ProjectReport {
    pub(crate) project_id: u64,
    pub(crate) identifier: String,
    pub(crate) ready: bool,
    pub(crate) memberships: Vec<MembershipReport>,
    pub(crate) error: Option<serde_json::Value>,
}

impl ProjectReport {
    pub(crate) fn new(
        project_id: u64,
        identifier: String,
        memberships: Vec<MembershipReport>,
        error: Option<serde_json::Value>,
    ) -> Self {
        let ready = error.is_none()
            && memberships
                .iter()
                .all(|membership| membership_actionable(&membership.status));
        Self {
            project_id,
            identifier,
            ready,
            memberships,
            error,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct SummaryReport {
    pub(crate) projects: usize,
    pub(crate) added: usize,
    pub(crate) updated: usize,
    pub(crate) existing: usize,
    pub(crate) planned: usize,
    pub(crate) warnings: usize,
    pub(crate) errors: usize,
}

impl SummaryReport {
    pub(crate) fn from_report(projects: &[ProjectReport]) -> Self {
        let mut summary = Self {
            projects: projects.len(),
            ..Self::default()
        };
        for project in projects {
            if project.error.is_some() {
                summary.errors += 1;
            }
            for membership in &project.memberships {
                match membership.status.as_str() {
                    STATUS_ADDED => summary.added += 1,
                    STATUS_UPDATED => summary.updated += 1,
                    STATUS_EXISTING => summary.existing += 1,
                    STATUS_PLANNED => summary.planned += 1,
                    STATUS_WARNING | STATUS_BLOCKED => summary.warnings += 1,
                    _ => {}
                }
            }
        }
        summary
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct GlobalBootstrapReport {
    pub(crate) mode: String,
    pub(crate) dry_run: bool,
    pub(crate) ready: bool,
    pub(crate) roles: Vec<RoleIdentityReport>,
    pub(crate) projects: Vec<ProjectReport>,
    pub(crate) summary: SummaryReport,
}

impl GlobalBootstrapReport {
    pub(crate) fn new(
        dry_run: bool,
        roles: Vec<RoleIdentityReport>,
        projects: Vec<ProjectReport>,
    ) -> Self {
        let ready = projects.iter().all(|project| project.ready);
        let summary = SummaryReport::from_report(&projects);
        Self {
            mode: MODE_ALL_PROJECTS.to_owned(),
            dry_run,
            ready,
            roles,
            projects,
            summary,
        }
    }

    /// `0` when every project is ready, `1` when any project is blocked,
    /// warned, or errored.
    pub(crate) fn exit_code(&self) -> i32 {
        if self.ready { 0 } else { 1 }
    }
}

/// The four built-in service identities in provision order, unresolved:
/// used when the complete project set is empty and no provider identity
/// read or write happens at all.
pub(crate) fn planned_role_identities(
    roles: impl IntoIterator<Item = (crate::policy::Role, &'static str, &'static str)>,
) -> Vec<RoleIdentityReport> {
    roles
        .into_iter()
        .map(|(role, login, role_name)| RoleIdentityReport {
            role: role.as_str().to_owned(),
            login: login.to_owned(),
            user_id: None,
            role_name: role_name.to_owned(),
            status: STATUS_PLANNED.to_owned(),
        })
        .collect()
}
