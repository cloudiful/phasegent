//! Non-secret config mutations and secure credential handling.
//!
//! Non-secret settings flow through the existing config write facade
//! ([`crate::config`]); role/provider credentials use the dedicated
//! storage path (write-only inputs, presence/length-only outputs).
//! Provisioning status reports the admin-provisioned Redmine identity
//! without ever exposing API keys.

use super::models::{
    ClearCredentialRequest, ClearCredentialResponse, ClearSettingRequest, ClearSettingResponse,
    CredentialPresence, ProvisioningQuery, ProvisioningStatus, SetCredentialRequest,
    SetSettingRequest, SetSettingResponse,
};
use super::validate::{
    bound_message, canonical_non_secret_setting, parse_provider_required, parse_role_optional,
    parse_role_required, validate_credential_value, validate_setting_value,
};

/// Non-secret setting write through the existing config facade.
#[allow(dead_code)]
pub fn write_setting_blocking(request: SetSettingRequest) -> Result<SetSettingResponse, String> {
    let canonical = canonical_non_secret_setting(&request.setting)?;
    let role = parse_role_optional(request.role.as_deref())?;
    if crate::config_write::is_role_scoped_setting(canonical) && role.is_none() {
        return Err(format!("--role is required for setting '{canonical}'"));
    }
    let value = validate_setting_value(canonical, &request.value)?;
    let storage = crate::infra::storage::Storage::open().map_err(bound_message)?;
    let outcome = crate::config::set_json(role, canonical, Some(&value), false, &storage)
        .map_err(bound_message)?;
    let updated = outcome
        .get("updated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    Ok(SetSettingResponse {
        setting: canonical.to_owned(),
        role: role.map(|r| r.as_str().to_owned()),
        updated,
    })
}

/// Non-secret setting clear through the existing facade.
#[allow(dead_code)]
pub fn clear_setting_blocking(
    request: ClearSettingRequest,
) -> Result<ClearSettingResponse, String> {
    let trimmed = request.setting.trim();
    if trimmed.is_empty() {
        return Err("setting cannot be empty".to_owned());
    }
    let canonical = crate::config_write::canonical_setting_name(trimmed)
        .ok_or_else(|| bound_message(format!("unknown setting '{trimmed}'")))?;
    if crate::config_write::is_secret_setting(canonical) {
        return Err(format!(
            "secret setting '{canonical}' must use the credential path"
        ));
    }
    let role = parse_role_optional(request.role.as_deref())?;
    let storage = crate::infra::storage::Storage::open().map_err(bound_message)?;
    let outcome = crate::config::clear_json(role, canonical, &storage).map_err(bound_message)?;
    let cleared = outcome
        .get("cleared")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(ClearSettingResponse {
        setting: canonical.to_owned(),
        role: role.map(|r| r.as_str().to_owned()),
        cleared,
    })
}

/// Secure credential set (write-only; response is presence/length).
#[allow(dead_code)]
pub fn write_credential_blocking(
    request: SetCredentialRequest,
) -> Result<CredentialPresence, String> {
    let role = parse_role_required(&request.role)?;
    let provider = parse_provider_required(&request.provider)?;
    let credential = validate_credential_value(&request.credential)?;
    let storage = crate::infra::storage::Storage::open().map_err(bound_message)?;
    storage
        .save_credential(role, provider.as_str(), &credential)
        .map_err(bound_message)?;
    let identity = storage
        .credential_summary(role, provider.as_str())
        .map_err(bound_message)?;
    Ok(CredentialPresence {
        role: role.as_str().to_owned(),
        provider: provider.as_str().to_owned(),
        present: true,
        length: identity.length,
        source: "storage".to_owned(),
    })
}

/// Secure credential clear.
#[allow(dead_code)]
pub fn clear_credential_blocking(
    request: ClearCredentialRequest,
) -> Result<ClearCredentialResponse, String> {
    let role = parse_role_required(&request.role)?;
    let provider = parse_provider_required(&request.provider)?;
    let storage = crate::infra::storage::Storage::open().map_err(bound_message)?;
    let had = storage
        .load_credential(role, provider.as_str())
        .map_err(bound_message)?
        .is_some();
    if had {
        storage
            .delete_credential(role, provider.as_str())
            .map_err(bound_message)?;
    }
    Ok(ClearCredentialResponse {
        role: role.as_str().to_owned(),
        provider: provider.as_str().to_owned(),
        cleared: had,
    })
}

/// Redmine provisioning status (identity only, never API keys).
#[allow(dead_code)]
pub fn read_provisioning_blocking(query: ProvisioningQuery) -> Result<ProvisioningStatus, String> {
    let role = parse_role_required(&query.role)?;
    let storage = crate::infra::storage::Storage::open().map_err(bound_message)?;
    let identity = crate::auth::load_redmine_user(role, &storage).map_err(bound_message)?;
    let identity_summary = storage
        .credential_summary(role, "redmine")
        .map_err(bound_message)?;
    Ok(ProvisioningStatus {
        role: role.as_str().to_owned(),
        user_id: identity.as_ref().map(|(id, _)| *id),
        login: identity.map(|(_, login)| login),
        credential_present: identity_summary.present,
        credential_length: identity_summary.length,
    })
}
