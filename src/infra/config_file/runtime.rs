//! Runtime configuration authority over the JSONC configuration file.
//!
//! Every configuration and credential read and write in phasegent goes
//! through this module. The JSONC file is the single authority; SQLite
//! keeps business state only.
//!
//! Reads never create the file: a missing configuration resolves to an
//! empty document. Writes take the sibling advisory lock, re-read the
//! current bytes, apply the change through the comment-preserving CST,
//! and rename the result over the file atomically. A concurrent external
//! edit is refused rather than overwritten.
//!
//! Environment variables stay runtime-only overrides. They are consulted
//! by the resolvers, never written here, and never ingested by the
//! migration importer.

use super::file::ConfigFileStore;
use super::model::{
    ConfigFile, ForgejoRoleConfig, GitlabRoleConfig, RedmineRoleConfig, RedmineUser,
    RoleLegacyConfig, SettingValue,
};
use crate::auth::{RedmineStoredConfig, StoredConfig};
use crate::policy::Role;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Non-secret identity of a stored credential: presence, length, the
/// last-4-characters fingerprint, and the epoch second it was stored.
/// The fingerprint deliberately reveals at most 4 characters (and
/// nothing for secrets shorter than 4) so `config show` and `doctor`
/// can answer "which key is this" without loading the secret for
/// display. Short secrets yield `fingerprint: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialIdentity {
    pub present: bool,
    pub length: usize,
    pub fingerprint: Option<String>,
    pub updated_at: Option<i64>,
}

/// Last-4-characters fingerprint of a secret. Returns `None` for
/// secrets shorter than 4 characters rather than echoing most of the
/// secret back.
pub fn credential_fingerprint(secret: &str) -> Option<String> {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 4 {
        return None;
    }
    Some(chars[chars.len() - 4..].iter().collect())
}

/// Provider-owned Forgejo settings and their provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgejoStoredConfig {
    pub api_base: String,
    pub repository: String,
    pub source: String,
    pub updated_at: i64,
}

/// Provider-owned GitLab settings and their provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitlabStoredConfig {
    pub api_base: String,
    pub repository: Option<String>,
    pub source: Option<String>,
    pub updated_at: Option<i64>,
}

/// The configuration file store for the current environment.
pub fn store() -> Result<ConfigFileStore, String> {
    ConfigFileStore::resolve()
}

/// The resolved configuration file path. Reported by `config show` and
/// `doctor` so operators can tell the configuration file apart from the
/// business database.
pub fn config_path() -> Result<PathBuf, String> {
    Ok(store()?.path().to_path_buf())
}

fn read_model() -> Result<ConfigFile, String> {
    Ok(store()?.load()?.model().clone())
}

fn now_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Read one machine-wide setting in its typed form. Returns `Ok(None)`
/// when the setting was never written.
pub fn load_setting(path: &str) -> Result<Option<SettingValue>, String> {
    Ok(read_model()?.setting(path))
}

/// Read one machine-wide setting in the flat text form. Returns
/// `Ok(None)` when the setting was never written, so callers can keep
/// the env-over-file precedence uniform.
///
/// A configuration file this build cannot read is an error here: the
/// resolution must never degrade into "not configured", because that
/// would silently change the effective provider and could route a write
/// to a different provider than the file names.
pub fn load_global_setting(path: &str) -> Result<Option<String>, String> {
    Ok(read_model()?.setting(path).map(|value| value.to_text()))
}

/// Persist one machine-wide setting. The flat text form is parsed into
/// the typed value the path declares, so the file keeps its JSON type.
/// A rejected value never reaches the file.
pub fn save_global_setting(path: &str, value: &str) -> Result<(), String> {
    let setting = super::setting_path::global_setting(path)
        .ok_or_else(|| "unknown configuration setting".to_owned())?;
    let typed = super::model::parse_setting_value(setting.kind, value)?;
    store()?.update(|document| document.set_setting(path, &typed))?;
    Ok(())
}

/// Remove one machine-wide setting. Returns `true` when a value was
/// actually removed so callers can distinguish a real clear from a no-op.
pub fn delete_global_setting(path: &str) -> Result<bool, String> {
    let mut cleared = false;
    store()?.update(|document| {
        cleared = document.clear_setting(path)?;
        Ok(())
    })?;
    Ok(cleared)
}

/// Set only the role-scoped provider preference, leaving the other
/// role-scoped values untouched.
pub fn update_provider(role: Role, provider: &str) -> Result<(), String> {
    store()?.update(|document| document.set_role_provider(role, Some(provider)))?;
    Ok(())
}

/// Load the role-level configuration: the provider preference plus the
/// inert legacy `api_base`/`repository` values. Returns `None` when the
/// role carries no configuration at all.
///
/// A configuration file this build cannot read is an error here: the
/// role-scoped provider is a provider selection, and reporting it as
/// absent would silently change the effective provider.
pub fn load_role_config(role: Role) -> Result<Option<StoredConfig>, String> {
    Ok(role_config_of(&read_model()?, role))
}

/// The role-level configuration carried by a parsed model.
fn role_config_of(model: &ConfigFile, role: Role) -> Option<StoredConfig> {
    let entry = model.role(role)?;
    if entry.provider.is_none()
        && entry.legacy.api_base.is_none()
        && entry.legacy.repository.is_none()
    {
        return None;
    }
    Some(StoredConfig {
        provider: entry.provider.clone(),
        api_base: entry.legacy.api_base.clone(),
        repository: entry.legacy.repository.clone(),
    })
}

/// Upsert the role-level configuration.
pub fn save_role_config(role: Role, config: &StoredConfig) -> Result<(), String> {
    store()?.update(|document| {
        document.set_role_provider(role, config.provider.as_deref())?;
        document.set_role_legacy(
            role,
            RoleLegacyConfig {
                api_base: config.api_base.clone(),
                repository: config.repository.clone(),
            },
        )
    })?;
    Ok(())
}

/// Load the Redmine-specific configuration for a role.
pub fn load_redmine_config(role: Role) -> Result<Option<RedmineStoredConfig>, String> {
    let model = read_model()?;
    let Some(redmine) = model.redmine(role) else {
        return Ok(None);
    };
    Ok(Some(RedmineStoredConfig {
        api_base: redmine.api_base.clone(),
        project_id: redmine.project_id.clone(),
        close_status_id: redmine.close_status_id,
        group_name: None,
        group_role: None,
    }))
}

/// Upsert the Redmine-specific configuration of a role.
pub fn save_redmine_config(role: Role, config: &RedmineStoredConfig) -> Result<(), String> {
    store()?.update(|document| {
        document.set_role_redmine_config(
            role,
            RedmineRoleConfig {
                api_base: config.api_base.clone(),
                project_id: config.project_id.clone(),
                close_status_id: config.close_status_id,
            },
        )
    })?;
    Ok(())
}

/// Set the bootstrap identity without disturbing an existing provider
/// preference. The `api_base` and `project_id` arguments are retained
/// for backward-compatible call sites but are ignored: the Redmine REST
/// address is canonical and global, and the project id is no longer
/// persisted.
pub fn persist_redmine_bootstrap(
    role: Role,
    api_base: Option<String>,
    project_id: u64,
    close_status_id: u64,
) -> Result<(), String> {
    if project_id == 0 || close_status_id == 0 {
        return Err("Redmine project and close status IDs must be greater than zero".to_owned());
    }
    let _ = api_base;
    let _ = project_id;
    store()?.update(|document| {
        document.set_role_redmine_config(
            role,
            RedmineRoleConfig {
                api_base: None,
                project_id: None,
                close_status_id: Some(close_status_id),
            },
        )?;
        document.set_role_provider(role, Some(crate::infra::storage_schema::PROVIDER_REDMINE))
    })?;
    Ok(())
}

/// Load the admin-provisioned Redmine identity for a role. Returns
/// `None` when the role was never provisioned.
pub fn load_redmine_user(role: Role) -> Result<Option<(u64, String)>, String> {
    let model = read_model()?;
    Ok(model
        .redmine_user(role)
        .map(|user| (user.user_id, user.login.clone())))
}

/// Upsert the admin-provisioned Redmine identity for a role.
pub fn save_redmine_user(role: Role, user_id: u64, login: &str) -> Result<(), String> {
    let user = RedmineUser {
        user_id,
        login: login.trim().to_owned(),
    };
    store()?.update(|document| document.set_role_redmine_user(role, user))?;
    Ok(())
}

/// Read the credential stored for `(role, provider)`.
pub fn load_credential(role: Role, provider: &str) -> Result<Option<String>, String> {
    Ok(read_model()?
        .credential(role, provider)
        .map(|entry| entry.value.clone()))
}

/// Store the credential for `(role, provider)`, overwriting any existing
/// value. The credential is stored verbatim and never surfaced in
/// errors; callers trim and reject empty input before invoking this.
pub fn save_credential(role: Role, provider: &str, credential: &str) -> Result<(), String> {
    let updated_at = now_epoch_seconds();
    store()?
        .update(|document| document.set_credential(role, provider, credential, Some(updated_at)))?;
    Ok(())
}

/// Describe the credential stored for `(role, provider)` without
/// surfacing the value itself.
pub fn credential_summary(role: Role, provider: &str) -> Result<CredentialIdentity, String> {
    let model = read_model()?;
    let Some(entry) = model.credential(role, provider) else {
        return Ok(CredentialIdentity {
            present: false,
            length: 0,
            fingerprint: None,
            updated_at: None,
        });
    };
    Ok(CredentialIdentity {
        present: true,
        length: entry.value.chars().count(),
        fingerprint: credential_fingerprint(&entry.value),
        updated_at: entry.updated_at,
    })
}

/// Load the provider-owned Forgejo configuration of a role.
pub fn load_forgejo_config(role: Role) -> Result<Option<ForgejoStoredConfig>, String> {
    let model = read_model()?;
    Ok(model.forgejo(role).map(|config| ForgejoStoredConfig {
        api_base: config.api_base.clone(),
        repository: config.repository.clone(),
        source: config.source.clone(),
        updated_at: config.updated_at,
    }))
}

/// Store the provider-owned Forgejo configuration and the matching
/// credential in one locked write.
pub fn save_forgejo_auth(
    role: Role,
    api_base: &str,
    repository: &str,
    credential: &str,
) -> Result<(), String> {
    let now = now_epoch_seconds();
    store()?.update(|document| {
        document.set_role_forgejo_config(
            role,
            ForgejoRoleConfig {
                api_base: api_base.to_owned(),
                repository: repository.to_owned(),
                source: "admin-auth-setup".to_owned(),
                updated_at: now,
            },
        )?;
        document.set_credential(role, "forgejo", credential, Some(now))
    })?;
    Ok(())
}

/// Load the provider-owned GitLab configuration of a role.
pub fn load_gitlab_config(role: Role) -> Result<Option<GitlabStoredConfig>, String> {
    let model = read_model()?;
    Ok(model.gitlab(role).map(|config| GitlabStoredConfig {
        api_base: config.api_base.clone().unwrap_or_default(),
        repository: config.repository.clone(),
        source: config.source.clone(),
        updated_at: config.updated_at,
    }))
}

/// Store the provider-owned GitLab configuration and the matching
/// credential in one locked write.
pub fn save_gitlab_auth(
    role: Role,
    api_base: &str,
    repository: &str,
    credential: &str,
) -> Result<(), String> {
    let now = now_epoch_seconds();
    store()?.update(|document| {
        document.set_role_gitlab_config(
            role,
            GitlabRoleConfig {
                api_base: Some(api_base.to_owned()),
                project_id: None,
                repository: Some(repository.to_owned()),
                source: Some("admin-auth-setup".to_owned()),
                updated_at: Some(now),
            },
        )?;
        document.set_credential(role, "gitlab", credential, Some(now))
    })?;
    Ok(())
}
