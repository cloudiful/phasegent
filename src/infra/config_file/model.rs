use crate::infra::storage_schema::GLOBAL_SETTING_NAMES;
use crate::policy::Role;
use crate::providers::config::ProviderKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u32 = 1;
pub const ROLE_NAMES: &[&str] = &["admin", "orchestrator", "executor", "reviewer", "explore"];
pub const CREDENTIAL_PROVIDERS: &[&str] = &["redmine", "forgejo", "gitlab"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConfigFile {
    pub version: u32,
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub roles: BTreeMap<String, RoleConfig>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            settings: BTreeMap::new(),
            roles: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct RoleConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "RoleLegacyConfig::is_empty")]
    pub legacy: RoleLegacyConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redmine: Option<RedmineRoleConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forgejo: Option<ForgejoRoleConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gitlab: Option<GitlabRoleConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub credentials: BTreeMap<String, CredentialEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redmine_user: Option<RedmineUser>,
}

/// Inert `role_config.api_base`/`repository` columns kept only so migration
/// does not lose values that older databases may still carry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct RoleLegacyConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

impl RoleLegacyConfig {
    pub fn is_empty(&self) -> bool {
        self.api_base.is_none() && self.repository.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct RedmineRoleConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_status_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ForgejoRoleConfig {
    pub api_base: String,
    pub repository: String,
    pub source: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct GitlabRoleConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CredentialEntry {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RedmineUser {
    pub user_id: u64,
    pub login: String,
}

impl ConfigFile {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_setting(&self, name: &str) -> Option<&str> {
        self.settings.get(name).map(String::as_str)
    }

    pub fn set_setting(&mut self, name: &str, value: &str) -> Result<(), String> {
        let trimmed = validate_setting(name, value)?;
        self.settings.insert(name.to_owned(), trimmed);
        Ok(())
    }

    /// Store a global setting exactly as it was persisted.
    ///
    /// The migration importer uses this to carry a persisted value over
    /// byte for byte: `set_setting` normalises the value it stores, so a
    /// value with surrounding whitespace would be changed on the way in.
    /// The value is validated exactly like a normal write — the name must
    /// be known and the value must not be empty — but the stored value
    /// keeps every byte of the original.
    pub fn set_setting_raw(&mut self, name: &str, value: &str) -> Result<(), String> {
        validate_setting_value(name, value)?;
        self.settings.insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    pub fn clear_setting(&mut self, name: &str) -> bool {
        self.settings.remove(name).is_some()
    }

    pub fn role(&self, role: Role) -> Option<&RoleConfig> {
        self.roles.get(role.as_str())
    }

    pub fn role_config_mut(&mut self, role: Role) -> &mut RoleConfig {
        self.roles.entry(role.as_str().to_owned()).or_default()
    }

    pub fn provider(&self, role: Role) -> Option<&str> {
        self.role(role).and_then(|entry| entry.provider.as_deref())
    }

    pub fn set_role_provider(&mut self, role: Role, provider: Option<&str>) -> Result<(), String> {
        let validated = provider
            .map(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err("role provider cannot be empty".to_owned());
                }
                trimmed
                    .parse::<ProviderKind>()
                    .map(|kind| kind.as_str().to_owned())
                    .map_err(|_| "invalid role provider".to_owned())
            })
            .transpose()?;
        self.role_config_mut(role).provider = validated;
        Ok(())
    }

    pub fn credential(&self, role: Role, provider: &str) -> Option<&CredentialEntry> {
        self.role(role)
            .and_then(|entry| entry.credentials.get(provider))
    }

    pub fn set_credential(
        &mut self,
        role: Role,
        provider: &str,
        value: &str,
        updated_at: Option<i64>,
    ) -> Result<(), String> {
        if !CREDENTIAL_PROVIDERS.contains(&provider) {
            return Err("unknown credential provider".to_owned());
        }
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("the credential value cannot be empty".to_owned());
        }
        self.role_config_mut(role).credentials.insert(
            provider.to_owned(),
            CredentialEntry {
                value: trimmed.to_owned(),
                updated_at,
            },
        );
        Ok(())
    }

    /// Store a credential exactly as it was persisted.
    ///
    /// `set_credential` trims the value it stores, which would change a
    /// persisted secret. The migration importer uses this to carry the
    /// stored secret over byte for byte. The value is still rejected when
    /// it holds nothing but whitespace, and the provider must be known.
    /// The value never appears in an error.
    pub fn set_credential_raw(
        &mut self,
        role: Role,
        provider: &str,
        value: &str,
        updated_at: Option<i64>,
    ) -> Result<(), String> {
        if !CREDENTIAL_PROVIDERS.contains(&provider) {
            return Err("unknown credential provider".to_owned());
        }
        if value.trim().is_empty() {
            return Err("the credential value cannot be empty".to_owned());
        }
        self.role_config_mut(role).credentials.insert(
            provider.to_owned(),
            CredentialEntry {
                value: value.to_owned(),
                updated_at,
            },
        );
        Ok(())
    }

    pub fn redmine(&self, role: Role) -> Option<&RedmineRoleConfig> {
        self.role(role).and_then(|entry| entry.redmine.as_ref())
    }

    pub fn forgejo(&self, role: Role) -> Option<&ForgejoRoleConfig> {
        self.role(role).and_then(|entry| entry.forgejo.as_ref())
    }

    pub fn gitlab(&self, role: Role) -> Option<&GitlabRoleConfig> {
        self.role(role).and_then(|entry| entry.gitlab.as_ref())
    }

    pub fn redmine_user(&self, role: Role) -> Option<&RedmineUser> {
        self.role(role)
            .and_then(|entry| entry.redmine_user.as_ref())
    }

    /// Replace the role-scoped Redmine configuration.
    pub fn set_role_redmine_config(
        &mut self,
        role: Role,
        config: RedmineRoleConfig,
    ) -> Result<(), String> {
        validate_redmine_role_config(&config)?;
        self.role_config_mut(role).redmine = Some(config);
        Ok(())
    }

    /// Replace the role-scoped Forgejo configuration.
    pub fn set_role_forgejo_config(
        &mut self,
        role: Role,
        config: ForgejoRoleConfig,
    ) -> Result<(), String> {
        validate_forgejo_role_config(&config)?;
        self.role_config_mut(role).forgejo = Some(config);
        Ok(())
    }

    /// Replace the role-scoped GitLab configuration.
    pub fn set_role_gitlab_config(
        &mut self,
        role: Role,
        config: GitlabRoleConfig,
    ) -> Result<(), String> {
        validate_gitlab_role_config(&config)?;
        self.role_config_mut(role).gitlab = Some(config);
        Ok(())
    }

    /// Replace the admin-provisioned Redmine identity for a role.
    pub fn set_role_redmine_user(&mut self, role: Role, user: RedmineUser) -> Result<(), String> {
        validate_redmine_user(&user)?;
        self.role_config_mut(role).redmine_user = Some(user);
        Ok(())
    }

    /// Replace the inert legacy `api_base`/`repository` columns of a role.
    pub fn set_role_legacy(&mut self, role: Role, legacy: RoleLegacyConfig) {
        self.role_config_mut(role).legacy = legacy;
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported configuration version {}; expected {}",
                self.version, SCHEMA_VERSION
            ));
        }
        for (name, value) in &self.settings {
            validate_setting(name, value)?;
        }
        for (name, role) in &self.roles {
            if !ROLE_NAMES.contains(&name.as_str()) {
                return Err("unknown role".to_owned());
            }
            role.validate()?;
        }
        Ok(())
    }
}

/// Validate a setting.
///
/// The setting name comes from an untrusted file, so it is never echoed
/// back: a name can be an accidentally pasted secret.
/// Whether a setting name is one of the canonical global setting names.
///
/// The name is not echoed back by any caller: a name that comes from a
/// file can be an accidentally pasted secret.
pub fn is_known_setting(name: &str) -> bool {
    GLOBAL_SETTING_NAMES.contains(&name)
}

fn validate_setting(name: &str, value: &str) -> Result<String, String> {
    validate_setting_value(name, value)?;
    Ok(value.trim().to_owned())
}

/// Check a setting value without changing it.
///
/// The check is the same one the normalising setter applies, so a value
/// that is stored verbatim is exactly as valid as a value that is stored
/// normalised.
fn validate_setting_value(name: &str, value: &str) -> Result<(), String> {
    if !GLOBAL_SETTING_NAMES.contains(&name) {
        return Err("unknown configuration setting".to_owned());
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("value for '{name}' cannot be empty"));
    }
    match name {
        "PHASEGENT_DEFAULT_PROVIDER" => {
            trimmed
                .parse::<ProviderKind>()
                .map_err(|_| format!("invalid value for '{name}'"))?;
        }
        "PHASEGENT_INDEX_BACKEND" => {
            let lower = trimmed.to_ascii_lowercase();
            if lower != "sqlite" && lower != "postgres" {
                return Err(format!(
                    "invalid value for '{name}'; expected sqlite or postgres"
                ));
            }
        }
        "PHASEGENT_NOTIFY_ENABLED" => {
            if crate::config_write::parse_bool_literal(trimmed).is_none() {
                return Err(format!(
                    "invalid value for '{name}'; expected true or false"
                ));
            }
        }
        "PHASEGENT_NOTIFY_CHANNEL" => {
            crate::notifications::config::NotifyChannel::parse(trimmed)
                .map_err(|_| format!("invalid value for '{name}'"))?;
        }
        "PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE" => {
            let lower = trimmed.to_ascii_lowercase();
            if lower != "text" && lower != "markdown" {
                return Err(format!(
                    "invalid value for '{name}'; expected text or markdown"
                ));
            }
        }
        "PHASEGENT_NOTIFY_EMAIL_SMTP_PORT" => {
            let port = trimmed
                .parse::<u16>()
                .map_err(|_| format!("invalid value for '{name}'; expected 1-65535"))?;
            if port == 0 {
                return Err(format!("{name} must be greater than zero"));
            }
        }
        "PHASEGENT_NOTIFY_EMAIL_TLS" => {
            let lower = trimmed.to_ascii_lowercase();
            if !matches!(lower.as_str(), "auto" | "implicit" | "starttls" | "plain") {
                return Err(format!(
                    "invalid value for '{name}'; expected auto, implicit, starttls, or plain"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

impl RoleConfig {
    fn validate(&self) -> Result<(), String> {
        if let Some(provider) = &self.provider {
            let trimmed = provider.trim();
            if trimmed.is_empty() {
                return Err("role provider cannot be empty".to_owned());
            }
            trimmed
                .parse::<ProviderKind>()
                .map_err(|_| "invalid role provider".to_owned())?;
        }
        if let Some(redmine) = &self.redmine {
            validate_redmine_role_config(redmine)?;
        }
        if let Some(forgejo) = &self.forgejo {
            validate_forgejo_role_config(forgejo)?;
        }
        if let Some(gitlab) = &self.gitlab {
            validate_gitlab_role_config(gitlab)?;
        }
        for (provider, credential) in &self.credentials {
            if !CREDENTIAL_PROVIDERS.contains(&provider.as_str()) {
                return Err("unknown credential provider".to_owned());
            }
            if credential.value.trim().is_empty() {
                return Err("the credential value cannot be empty".to_owned());
            }
        }
        if let Some(user) = &self.redmine_user {
            validate_redmine_user(user)?;
        }
        Ok(())
    }
}

fn validate_redmine_role_config(config: &RedmineRoleConfig) -> Result<(), String> {
    if let Some(close_status_id) = config.close_status_id
        && close_status_id == 0
    {
        return Err("Redmine close status id must be greater than zero".to_owned());
    }
    Ok(())
}

fn validate_forgejo_role_config(config: &ForgejoRoleConfig) -> Result<(), String> {
    for (field, value) in [
        ("forgejo apiBase", config.api_base.as_str()),
        ("forgejo repository", config.repository.as_str()),
        ("forgejo source", config.source.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{field} cannot be empty"));
        }
    }
    Ok(())
}

fn validate_gitlab_role_config(config: &GitlabRoleConfig) -> Result<(), String> {
    if let Some(api_base) = config.api_base.as_deref()
        && api_base.trim().is_empty()
    {
        return Err("gitlab apiBase cannot be empty".to_owned());
    }
    if let Some(repository) = config.repository.as_deref()
        && repository.trim().is_empty()
    {
        return Err("gitlab repository cannot be empty".to_owned());
    }
    if let Some(source) = config.source.as_deref()
        && source.trim().is_empty()
    {
        return Err("gitlab source cannot be empty".to_owned());
    }
    Ok(())
}

fn validate_redmine_user(user: &RedmineUser) -> Result<(), String> {
    if user.user_id == 0 {
        return Err("Redmine user id must be greater than zero".to_owned());
    }
    if user.login.trim().is_empty() {
        return Err("Redmine user login cannot be empty".to_owned());
    }
    if user.login.chars().any(char::is_control) {
        return Err("Redmine user login contains invalid characters".to_owned());
    }
    Ok(())
}
