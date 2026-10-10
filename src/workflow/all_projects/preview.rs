//! Read-only all-projects preview.
//!
//! Enumerates every existing project's direct memberships and plans the
//! additive role reconciliation without issuing a single write: no user
//! creation, no API-key fetch, no membership/identity/settings persistence.

use super::identity::{ResolvedIdentity, resolve_identities};
use super::model::{
    GlobalBootstrapReport, MembershipReport, ProjectReport, RoleIdentityReport, STATUS_ADDED,
    STATUS_BLOCKED, STATUS_EXISTING, STATUS_PLANNED, STATUS_UPDATED, STATUS_WARNING,
};
use crate::providers::api::PhasegentError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{RedmineMembership, RedmineProject, RedmineRole};

enum RoleResolution {
    Found(u64),
    Missing,
    Ambiguous,
}

pub(super) fn preview(
    admin: &RedmineProvider,
    projects: &[RedmineProject],
) -> Result<GlobalBootstrapReport, PhasegentError> {
    let identities = resolve_identities(admin)?;
    let roles = admin.list_roles_readonly();
    let mut reports = Vec::with_capacity(projects.len());
    for project in projects {
        reports.push(match &roles {
            Ok(roles) => preview_project(admin, project, &identities, roles),
            Err(error) => project_role_read_failure(project, &identities, error),
        });
    }
    let role_reports = identities
        .iter()
        .map(|identity| RoleIdentityReport {
            role: identity.role.as_str().to_owned(),
            login: identity.login.clone(),
            user_id: identity.user.as_ref().map(|user| user.id),
            role_name: identity.role_name.to_owned(),
            status: identity
                .user
                .as_ref()
                .map_or(STATUS_PLANNED, |_| STATUS_EXISTING)
                .to_owned(),
        })
        .collect();
    Ok(GlobalBootstrapReport::new(true, role_reports, reports))
}

fn preview_project(
    admin: &RedmineProvider,
    project: &RedmineProject,
    identities: &[ResolvedIdentity],
    roles: &[RedmineRole],
) -> ProjectReport {
    let memberships = match admin.list_project_memberships(project.id) {
        Ok(memberships) => memberships,
        Err(error) => {
            let warning = error_message(&error);
            let blocked = identities
                .iter()
                .map(|identity| MembershipReport {
                    role: identity.role.as_str().to_owned(),
                    user_id: identity.user.as_ref().map(|user| user.id),
                    login: identity.login.clone(),
                    role_name: identity.role_name.to_owned(),
                    status: STATUS_BLOCKED.to_owned(),
                    warning: Some(warning.clone()),
                })
                .collect();
            return ProjectReport::new(
                project.id,
                project.identifier.clone(),
                blocked,
                Some(error.json()),
            );
        }
    };
    let plans = identities
        .iter()
        .map(|identity| plan_membership(identity, roles, &memberships))
        .collect();
    ProjectReport::new(project.id, project.identifier.clone(), plans, None)
}

fn project_role_read_failure(
    project: &RedmineProject,
    identities: &[ResolvedIdentity],
    error: &PhasegentError,
) -> ProjectReport {
    let warning = error_message(error);
    let memberships = identities
        .iter()
        .map(|identity| MembershipReport {
            role: identity.role.as_str().to_owned(),
            user_id: identity.user.as_ref().map(|user| user.id),
            login: identity.login.clone(),
            role_name: identity.role_name.to_owned(),
            status: STATUS_BLOCKED.to_owned(),
            warning: Some(warning.clone()),
        })
        .collect();
    ProjectReport::new(
        project.id,
        project.identifier.clone(),
        memberships,
        Some(error.json()),
    )
}

fn plan_membership(
    identity: &ResolvedIdentity,
    roles: &[RedmineRole],
    memberships: &[RedmineMembership],
) -> MembershipReport {
    let base = |status: &str, user_id: Option<u64>, warning: Option<String>| MembershipReport {
        role: identity.role.as_str().to_owned(),
        user_id,
        login: identity.login.clone(),
        role_name: identity.role_name.to_owned(),
        status: status.to_owned(),
        warning,
    };
    let role_id = match resolve_role(roles, identity.role_name) {
        RoleResolution::Found(id) => id,
        RoleResolution::Missing => {
            return base(
                STATUS_WARNING,
                identity.user.as_ref().map(|user| user.id),
                Some("role was not found".to_owned()),
            );
        }
        RoleResolution::Ambiguous => {
            return base(
                STATUS_WARNING,
                identity.user.as_ref().map(|user| user.id),
                Some("role is ambiguous".to_owned()),
            );
        }
    };
    let Some(user) = &identity.user else {
        // Missing deterministic user: planned creation, unknown Redmine id,
        // and never a preview API-key fetch.
        return base(STATUS_PLANNED, None, None);
    };
    match memberships.iter().find(|membership| {
        membership
            .user
            .as_ref()
            .is_some_and(|candidate| candidate.id == user.id)
    }) {
        None => base(STATUS_ADDED, Some(user.id), None),
        Some(membership) if membership.roles.iter().any(|role| role.id == role_id) => {
            base(STATUS_EXISTING, Some(user.id), None)
        }
        Some(_) => base(STATUS_UPDATED, Some(user.id), None),
    }
}

/// Match the write path's role-name semantics exactly: one exact name wins,
/// zero or many are a visible warning rather than a guess.
fn resolve_role(roles: &[RedmineRole], name: &str) -> RoleResolution {
    let matches = roles
        .iter()
        .filter(|role| role.name == name)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [role] => RoleResolution::Found(role.id),
        [] => RoleResolution::Missing,
        _ => RoleResolution::Ambiguous,
    }
}

pub(super) fn error_message(error: &PhasegentError) -> String {
    error
        .json()
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| error.to_string())
}
