//! All-projects apply.
//!
//! Provisions the four shared service identities exactly once (reusing
//! [`crate::workflow::provision::provision_agent_users`], including its
//! distinct-identity gate), then reconciles each project's direct
//! memberships additively. Per-project and per-role failures are collected so
//! the complete report is produced; a shared provisioning failure aborts
//! before any membership write.

use super::model::{
    GlobalBootstrapReport, MembershipReport, ProjectReport, RoleIdentityReport, STATUS_BLOCKED,
    STATUS_PROVISIONED,
};
use super::preview::error_message;
use crate::providers::api::PhasegentError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::RedmineProject;
use crate::workflow::provision::{ProvisionedRole, membership_role_name, provision_agent_users};

pub(super) fn apply(
    admin: &RedmineProvider,
    projects: &[RedmineProject],
) -> Result<GlobalBootstrapReport, PhasegentError> {
    let provisioned = provision_agent_users(admin)?;
    let mut role_reports = Vec::with_capacity(provisioned.len());
    for entry in &provisioned {
        role_reports.push(RoleIdentityReport {
            role: entry.role.as_str().to_owned(),
            login: entry.user.login.clone(),
            user_id: Some(entry.user.id),
            role_name: membership_role_name(entry.role)?.to_owned(),
            status: STATUS_PROVISIONED.to_owned(),
        });
    }
    let mut reports = Vec::with_capacity(projects.len());
    for project in projects {
        reports.push(apply_project(admin, project, &provisioned));
    }
    Ok(GlobalBootstrapReport::new(false, role_reports, reports))
}

fn apply_project(
    admin: &RedmineProvider,
    project: &RedmineProject,
    provisioned: &[ProvisionedRole],
) -> ProjectReport {
    let mut memberships = Vec::with_capacity(provisioned.len());
    let mut project_error = None;
    for entry in provisioned {
        let role_name = match membership_role_name(entry.role) {
            Ok(role_name) => role_name,
            Err(error) => {
                if project_error.is_none() {
                    project_error = Some(error.json());
                }
                memberships.push(blocked(entry, None, error_message(&error)));
                continue;
            }
        };
        match admin.ensure_user_membership(project.id, &entry.user, role_name) {
            Ok(outcome) => memberships.push(MembershipReport {
                role: entry.role.as_str().to_owned(),
                user_id: Some(outcome.user_id),
                login: outcome.user_login,
                role_name: outcome.role_name,
                status: outcome.status,
                warning: outcome.warning,
            }),
            Err(error) => {
                if project_error.is_none() {
                    project_error = Some(error.json());
                }
                memberships.push(blocked(
                    entry,
                    Some(role_name.to_owned()),
                    error_message(&error),
                ));
            }
        }
    }
    ProjectReport::new(
        project.id,
        project.identifier.clone(),
        memberships,
        project_error,
    )
}

fn blocked(
    entry: &ProvisionedRole,
    role_name: Option<String>,
    warning: String,
) -> MembershipReport {
    MembershipReport {
        role: entry.role.as_str().to_owned(),
        user_id: Some(entry.user.id),
        login: entry.user.login.clone(),
        role_name: role_name.unwrap_or_default(),
        status: STATUS_BLOCKED.to_owned(),
        warning: Some(warning),
    }
}
