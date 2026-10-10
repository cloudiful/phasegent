//! Shared role identity reads for the all-projects preview.

use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{
    RedmineCurrentUser, provisioned_roles, provisioning_metadata,
};
use crate::workflow::provision::{membership_role_name, persisted_role_identity};

pub(super) struct ResolvedIdentity {
    pub(super) role: Role,
    pub(super) login: String,
    pub(super) role_name: &'static str,
    pub(super) user: Option<RedmineCurrentUser>,
}

/// Resolve each persisted authoritative identity first, otherwise look up its
/// deterministic login without fetching an API key.
pub(super) fn resolve_identities(
    admin: &RedmineProvider,
) -> Result<Vec<ResolvedIdentity>, PhasegentError> {
    let storage = crate::infra::storage::Storage::open().map_err(PhasegentError::config)?;
    let mut resolved: Vec<ResolvedIdentity> = Vec::new();
    for role in provisioned_roles() {
        let metadata = provisioning_metadata(role).ok_or_else(|| {
            PhasegentError::config(format!(
                "no provisioning metadata for role {}",
                role.as_str()
            ))
        })?;
        let user = match persisted_role_identity(role, &storage)? {
            Some(user) => Some(user),
            None => admin
                .find_user_by_login(metadata.login)?
                .map(|user| RedmineCurrentUser {
                    id: user.id,
                    login: user.login,
                    firstname: user.firstname,
                    lastname: user.lastname,
                    mail: user.mail,
                }),
        };
        if let Some(user) = &user
            && let Some(existing) = resolved.iter().find(|entry| {
                entry
                    .user
                    .as_ref()
                    .is_some_and(|candidate| candidate.id == user.id)
            })
        {
            return Err(PhasegentError::config(format!(
                "Redmine role-scoped API keys must identify distinct users; \
                 got {}={} and {}=#{}",
                role.as_str(),
                user.login,
                existing.role.as_str(),
                user.id
            )));
        }
        let login = user
            .as_ref()
            .map_or(metadata.login, |user| user.login.as_str())
            .to_owned();
        resolved.push(ResolvedIdentity {
            role,
            login,
            role_name: membership_role_name(role)?,
            user,
        });
    }
    Ok(resolved)
}
