//! Role enumeration and the distinct-identity gate for bootstrap
//! provisioning.
//!
//! Everything here is driven by the shared
//! [`provisioned_roles`] / [`default_redmine_role`] /
//! [`provisioning_metadata`] registry in the Redmine model, so adding a
//! role never requires a second hardcoded list here.

use super::provision_user::provision_role_user;
use crate::policy::Role;
use crate::providers::RedmineProvider;
use crate::providers::api::PhasegentError;
use crate::providers::redmine::model::{
    RedmineCurrentUser, default_redmine_role, provisioned_roles, provisioning_metadata,
};

/// One provisioned agent role: the policy role and the Redmine identity the
/// administrator API resolved for it.
pub(super) struct ProvisionedRole {
    pub(super) role: Role,
    pub(super) user: RedmineCurrentUser,
}

/// Provision every built-in agent role through the administrator API.
///
/// Order follows [`provisioned_roles`] so membership reconciliation stays
/// deterministic. Each role reuses its persisted identity when both the
/// `role_redmine_user` row and the `role_credential` row are present;
/// otherwise the deterministic login is looked up before creating so
/// reruns and legacy databases never create duplicates.
///
/// Every newly provisioned identity is checked against the ones already
/// accepted here, so a collision aborts the run before any membership
/// mutation or bootstrap configuration write.
pub(super) fn provision_agent_users(
    admin: &RedmineProvider,
) -> Result<Vec<ProvisionedRole>, PhasegentError> {
    let storage = crate::infra::storage::Storage::open().map_err(PhasegentError::config)?;
    let roles = provisioned_roles();
    let mut provisioned: Vec<ProvisionedRole> = Vec::with_capacity(roles.len());
    for role in roles {
        let metadata = provisioning_metadata(role).ok_or_else(|| {
            PhasegentError::config(format!(
                "no provisioning metadata for role {}",
                role.as_str()
            ))
        })?;
        let user = provision_role_user(admin, &storage, role, &metadata)?;
        ensure_distinct(&provisioned, role, &user)?;
        provisioned.push(ProvisionedRole { role, user });
    }
    Ok(provisioned)
}

/// Reject a freshly provisioned identity that resolves to the same Redmine
/// user as an already accepted role.
fn ensure_distinct(
    provisioned: &[ProvisionedRole],
    role: Role,
    user: &RedmineCurrentUser,
) -> Result<(), PhasegentError> {
    let Some(colliding) = provisioned
        .iter()
        .find(|entry| entry.user.id == user.id)
        .map(|entry| entry.role)
    else {
        return Ok(());
    };
    Err(PhasegentError::config(format!(
        "Redmine role-scoped API keys must identify distinct users; \
         got {role}={user} and {colliding}={user}",
        role = role.as_str(),
        colliding = colliding.as_str(),
        user = describe_user(user),
    )))
}

/// Default Redmine role name for a provisioned agent role.
pub(super) fn membership_role_name(role: Role) -> Result<&'static str, PhasegentError> {
    default_redmine_role(role).ok_or_else(|| {
        PhasegentError::config(format!(
            "no default Redmine project role for role {}",
            role.as_str()
        ))
    })
}

fn describe_user(user: &RedmineCurrentUser) -> String {
    if !user.login.is_empty() {
        user.login.clone()
    } else {
        format!("#{}", user.id)
    }
}
