//! Per-role identity, API key, and persistence flow for one agent role.

use crate::auth;
use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::RedmineProvider;
use crate::providers::api::PhasegentError;
use crate::providers::redmine::model::{RedmineCurrentUser, RoleProvisioningMetadata};

/// Provision one agent role: reuse the persisted identity when present,
/// otherwise look up the deterministic login and create only when absent.
///
/// The administrator provider performs every HTTP call (lookup, create,
/// API-key read). No role-scoped key is read for identity, so a missing
/// admin key fails here without falling back to another role key. The
/// retrieved API key and the user identity are persisted before
/// returning; generated passwords never leave the server because
/// creation uses Redmine password generation.
pub(super) fn provision_role_user(
    admin: &RedmineProvider,
    storage: &Storage,
    role: Role,
    metadata: &RoleProvisioningMetadata,
) -> Result<RedmineCurrentUser, PhasegentError> {
    if let Some(user) = persisted_identity(role, storage)? {
        return Ok(user);
    }
    if let Some(existing) = admin.find_user_by_login(metadata.login).map_err(|error| {
        PhasegentError::config(format!(
            "could not lookup the {} user '{}': {}",
            role.as_str(),
            metadata.login,
            describe(&error)
        ))
    })? {
        return adopt_existing_user(admin, storage, role, existing);
    }

    let created = match admin.create_service_user(
        metadata.login,
        metadata.firstname,
        metadata.lastname,
        metadata.mail,
    ) {
        Ok(user) => user,
        Err(error) if is_duplicate_login(&error) => {
            let recovered = admin.find_user_by_login(metadata.login).map_err(|inner| {
                PhasegentError::config(format!(
                    "could not lookup the {} user '{}' after duplicate: {}",
                    role.as_str(),
                    metadata.login,
                    describe(&inner)
                ))
            })?;
            let existing = recovered.ok_or_else(|| {
                PhasegentError::config(format!(
                    "Redmine user '{}' already exists but lookup found nothing",
                    metadata.login
                ))
            })?;
            return adopt_existing_user(admin, storage, role, existing);
        }
        Err(error) => {
            return Err(PhasegentError::config(format!(
                "could not create the {} user '{}': {}",
                role.as_str(),
                metadata.login,
                describe(&error)
            )));
        }
    };
    let api_key = read_api_key(admin, role, created.id)?;
    auth::save_redmine_user(role, created.id, &created.login, storage)
        .map_err(PhasegentError::config)?;
    storage
        .save_credential(role, crate::infra::storage::PROVIDER_REDMINE, &api_key)
        .map_err(PhasegentError::config)?;
    Ok(RedmineCurrentUser {
        id: created.id,
        login: created.login,
        firstname: created.firstname,
        lastname: created.lastname,
        mail: created.mail,
    })
}

/// Reuse the identity already persisted for `role`.
///
/// Both the non-secret `role_redmine_user` row and a non-blank
/// `role_credential` row are required, so a half-written or legacy record
/// falls through to the admin lookup-and-create path instead of silently
/// reusing a stale identity.
fn persisted_identity(
    role: Role,
    storage: &Storage,
) -> Result<Option<RedmineCurrentUser>, PhasegentError> {
    let persisted_user = auth::load_redmine_user(role, storage).map_err(PhasegentError::config)?;
    let persisted_key = storage
        .load_credential(role, crate::infra::storage::PROVIDER_REDMINE)
        .map_err(PhasegentError::config)?;
    if let (Some((user_id, login)), Some(api_key)) = (persisted_user, persisted_key)
        && user_id > 0
        && !login.trim().is_empty()
        && !api_key.trim().is_empty()
    {
        return Ok(Some(RedmineCurrentUser {
            id: user_id,
            login,
            firstname: String::new(),
            lastname: String::new(),
            mail: String::new(),
        }));
    }
    Ok(None)
}

/// Read the API key of an already existing user and persist the pairing.
fn adopt_existing_user(
    admin: &RedmineProvider,
    storage: &Storage,
    role: Role,
    existing: crate::providers::redmine::model::RedmineUser,
) -> Result<RedmineCurrentUser, PhasegentError> {
    let api_key = read_api_key(admin, role, existing.id)?;
    auth::save_redmine_user(role, existing.id, &existing.login, storage)
        .map_err(PhasegentError::config)?;
    storage
        .save_credential(role, crate::infra::storage::PROVIDER_REDMINE, &api_key)
        .map_err(PhasegentError::config)?;
    Ok(RedmineCurrentUser {
        id: existing.id,
        login: existing.login,
        firstname: existing.firstname,
        lastname: existing.lastname,
        mail: existing.mail,
    })
}

fn read_api_key(
    admin: &RedmineProvider,
    role: Role,
    user_id: u64,
) -> Result<String, PhasegentError> {
    admin.get_user_api_key(user_id).map_err(|error| {
        PhasegentError::config(format!(
            "could not retrieve the {} user API key: {}",
            role.as_str(),
            describe(&error)
        ))
    })
}

fn is_duplicate_login(error: &PhasegentError) -> bool {
    match error {
        PhasegentError::Http {
            status: 422,
            message,
            ..
        } => {
            let lower = message.to_ascii_lowercase();
            lower.contains("already been taken")
                || lower.contains("has already")
                || lower.contains("duplicate")
        }
        _ => false,
    }
}

fn describe(error: &PhasegentError) -> String {
    let json = error.json();
    json.get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| error.to_string())
}
