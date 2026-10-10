//! Explicit global Redmine bootstrap across every existing project.
//!
//! The no-flag `bootstrap` keeps the current-repository flow. This module owns
//! the `--all-projects` path: enumerate the complete project set, then either
//! preview it read-only or reconcile the four shared service identities'
//! additive direct memberships. It never creates projects or mirrors, installs
//! hooks, or writes close-status/provider/project settings or the completed
//! bootstrap cache.

mod apply;
mod identity;
mod model;
mod preview;

pub(crate) use model::GlobalBootstrapReport;
#[cfg(test)]
pub(crate) use model::{MembershipReport, ProjectReport, RoleIdentityReport};

use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{RedmineProject, provisioned_roles, provisioning_metadata};
use crate::workflow::provision::membership_role_name;
use crate::workflow::repository::resolve_bootstrap_config;
use std::collections::BTreeMap;

/// Run the global all-projects bootstrap (or its read-only preview).
///
/// Fatal errors — project enumeration, shared identity resolution, and shared
/// provisioning — return here before any membership mutation. A complete empty
/// project set is a ready no-op that performs no identity read or write.
pub(crate) fn run(
    api_base: Option<&str>,
    dry_run: bool,
) -> Result<GlobalBootstrapReport, PhasegentError> {
    let config = resolve_bootstrap_config(Role::Admin, api_base, None)?;
    let admin = RedmineProvider::for_role(Role::Admin, config)?;
    let projects = complete_projects(&admin)?;
    if projects.is_empty() {
        return Ok(GlobalBootstrapReport::new(
            dry_run,
            planned_role_identities()?,
            Vec::new(),
        ));
    }
    if dry_run {
        preview::preview(&admin, &projects)
    } else {
        apply::apply(&admin, &projects)
    }
}

/// Enumerate the complete project set, then deduplicate by id deterministically
/// (ascending). A duplicate id with a conflicting identifier is contradictory
/// and rejected instead of silently collapsed.
fn complete_projects(admin: &RedmineProvider) -> Result<Vec<RedmineProject>, PhasegentError> {
    let projects = admin.list_projects()?;
    let mut by_id: BTreeMap<u64, RedmineProject> = BTreeMap::new();
    for project in projects {
        match by_id.get(&project.id) {
            Some(existing) if existing.identifier != project.identifier => {
                return Err(PhasegentError::config(format!(
                    "Redmine returned conflicting projects for id {}: '{}' and '{}'",
                    project.id, existing.identifier, project.identifier
                )));
            }
            Some(_) => {}
            None => {
                by_id.insert(project.id, project);
            }
        }
    }
    Ok(by_id.into_values().collect())
}

/// The four built-in identities as unresolved `planned` entries for the empty
/// project set, so a no-op run still reports the global role surface without
/// any provider read.
fn planned_role_identities() -> Result<Vec<model::RoleIdentityReport>, PhasegentError> {
    let mut roles = Vec::new();
    for role in provisioned_roles() {
        let metadata = provisioning_metadata(role).ok_or_else(|| {
            PhasegentError::config(format!(
                "no provisioning metadata for role {}",
                role.as_str()
            ))
        })?;
        roles.push((role, metadata.login, membership_role_name(role)?));
    }
    Ok(model::planned_role_identities(roles))
}
