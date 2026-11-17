//! The typed schema-v2 configuration model.
//!
//! The configuration file carries a hierarchy of typed values: a machine-wide
//! `provider`, a `redmine` section, an `index` section, a `notifications`
//! section, and the per-role entries. Every value keeps its JSON type: a
//! boolean stays a boolean, a port stays a number, a recipient list stays an
//! array. There is no flat settings map and no environment name in the file.
//!
//! Absent values stay absent. A section that carries nothing is the same as
//! a missing section, so the file never gains a default it did not hold.

use crate::notifications::config::NotifyChannel;
use crate::policy::Role;
use crate::providers::config::ProviderKind;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u32 = 2;
pub const ROLE_NAMES: &[&str] = &["admin", "orchestrator", "executor", "reviewer", "explore"];
pub const CREDENTIAL_PROVIDERS: &[&str] = &["redmine", "forgejo", "gitlab"];

/// The value kinds the configuration tree carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingKind {
    /// A non-empty string.
    Text,
    /// A boolean.
    Bool,
    /// A TCP port between 1 and 65535.
    Port,
    /// A list of non-empty strings.
    List,
    /// A string from a fixed set of literals.
    Choice(&'static [&'static str]),
}

/// One configuration value in its typed form.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Text(String),
    Bool(bool),
    Port(u16),
    List(Vec<String>),
}

impl SettingValue {
    /// The textual form the flat setting interface uses. The
    /// configuration file itself never stores this form; it is the
    /// compatibility shape the environment overrides and the historical
    /// CLI aliases speak.
    pub fn to_text(&self) -> String {
        match self {
            Self::Text(value) => value.clone(),
            Self::Bool(value) => value.to_string(),
            Self::Port(value) => value.to_string(),
            Self::List(values) => values.join(","),
        }
    }

    /// The JSON shape of the value, used by the snapshot so a typed value
    /// stays typed on the way out.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Text(value) => serde_json::Value::String(value.clone()),
            Self::Bool(value) => serde_json::Value::Bool(*value),
            Self::Port(value) => serde_json::Value::from(*value),
            Self::List(values) => serde_json::Value::Array(
                values
                    .iter()
                    .map(|value| serde_json::Value::String(value.clone()))
                    .collect(),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConfigFile {
    pub version: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub provider: Option<ProviderKind>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub redmine: Option<RedmineSettings>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub index: Option<IndexSettings>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub notifications: Option<NotificationSettings>,
    #[serde(default)]
    pub roles: BTreeMap<String, RoleConfig>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            provider: None,
            redmine: None,
            index: None,
            notifications: None,
            roles: BTreeMap::new(),
        }
    }
}

/// The machine-wide Redmine section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RedmineSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub api_base: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub repository_url: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub git_mirror: Option<GitMirrorSettings>,
}

/// The Redmine git-mirror section. It carries the mirror bearer key.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitMirrorSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub api_key: Option<String>,
}

/// The issue-index section. The PostgreSQL URL decides the backend: its
/// presence selects PostgreSQL, its absence selects SQLite.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IndexSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub postgres: Option<PostgresSettings>,
}

/// The PostgreSQL index settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PostgresSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub url: Option<String>,
}

/// The agent-notification section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NotificationSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub channel: Option<NotifyChannel>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub ntfy: Option<NtfySettings>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub webhook: Option<WebhookSettings>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub dingtalk: Option<DingtalkSettings>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub email: Option<EmailSettings>,
}

/// The ntfy notification settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NtfySettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub base_url: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub topic: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub token: Option<String>,
}

/// The webhook notification settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WebhookSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub url: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub token: Option<String>,
}

/// The DingTalk notification settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DingtalkSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub webhook_url: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub secret: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "de_list"
    )]
    pub keywords: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub message_type: Option<DingtalkMessageType>,
}

/// The DingTalk message type.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DingtalkMessageType {
    Text,
    Markdown,
}

/// The e-mail notification settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EmailSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_section"
    )]
    pub smtp: Option<SmtpSettings>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub username: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub password: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub from: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "de_list"
    )]
    pub to: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub reply_to: Option<String>,
}

/// The SMTP transport settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SmtpSettings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_text"
    )]
    pub host: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_port"
    )]
    pub port: Option<u16>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub tls: Option<SmtpTls>,
}

/// The SMTP transport security mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpTls {
    Auto,
    Implicit,
    Starttls,
    Plain,
}

fn de_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn de_port<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    let port = u16::deserialize(deserializer)?;
    validate_value(SettingKind::Port, &SettingValue::Port(port))
        .map_err(serde::de::Error::custom)?;
    Ok(Some(port))
}

fn de_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    validate_value(SettingKind::List, &SettingValue::List(values.clone()))
        .map_err(serde::de::Error::custom)?;
    Ok(values)
}

/// Deserialize a text field. A present value must be a string and must not
/// be empty: the configuration never stores a null or an empty string where
/// a value belongs.
fn de_text<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        return Err(serde::de::Error::custom("value cannot be empty"));
    }
    Ok(Some(value))
}

/// Deserialize a nested section. A section that carries nothing is the same
/// as a missing section.
fn de_section<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default + PartialEq,
{
    let value = T::deserialize(deserializer)?;
    Ok(if value == T::default() {
        None
    } else {
        Some(value)
    })
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct RoleConfig {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "RoleLegacyConfig::is_empty")]
    pub legacy: RoleLegacyConfig,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub redmine: Option<RedmineRoleConfig>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub forgejo: Option<ForgejoRoleConfig>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub gitlab: Option<GitlabRoleConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub credentials: BTreeMap<String, CredentialEntry>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub redmine_user: Option<RedmineUser>,
}

/// Inert `role_config.api_base`/`repository` columns kept only so migration
/// does not lose values that older databases may still carry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct RoleLegacyConfig {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub api_base: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
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
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub api_base: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub project_id: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
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
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub api_base: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub project_id: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub repository: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub source: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
    pub updated_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CredentialEntry {
    pub value: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_present"
    )]
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

    /// Read one machine-wide setting in its typed form.
    pub fn setting(&self, path: &str) -> Option<SettingValue> {
        let setting = super::setting_path::global_setting(path)?;
        let document = serde_json::to_value(self).ok()?;
        let mut current = &document;
        for segment in setting.segments {
            current = current.get(segment)?;
        }
        value_from_json(setting.kind, current)
    }

    /// Store one machine-wide setting. The value is validated against the
    /// kind the path declares, so an out-of-range port or an unknown
    /// literal never reaches the file.
    pub fn set_setting(&mut self, path: &str, value: SettingValue) -> Result<(), String> {
        let setting = super::setting_path::global_setting(path)
            .ok_or_else(|| "unknown configuration setting".to_owned())?;
        validate_value(setting.kind, &value)?;
        let mut document = serde_json::to_value(&*self)
            .map_err(|_| "could not read the configuration".to_owned())?;
        set_json_path(&mut document, setting.segments, &value)?;
        let model: ConfigFile = serde_json::from_value(document)
            .map_err(|_| "the value is not valid for this configuration field".to_owned())?;
        *self = model;
        Ok(())
    }

    /// Remove one machine-wide setting. Returns `true` when a value was
    /// actually removed.
    pub fn clear_setting(&mut self, path: &str) -> bool {
        let Some(setting) = super::setting_path::global_setting(path) else {
            return false;
        };
        let Ok(mut document) = serde_json::to_value(&*self) else {
            return false;
        };
        if !remove_json_path(&mut document, setting.segments) {
            return false;
        }
        let Ok(model) = serde_json::from_value::<ConfigFile>(document) else {
            return false;
        };
        *self = model;
        true
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
            if self.version == 1 {
                return Err(
                    "the configuration file uses schema version 1; a human operator must \
                     upgrade it with `phasegent admin config upgrade` before phasegent \
                     reads or writes it"
                        .to_owned(),
                );
            }
            return Err(format!(
                "unsupported configuration version {}; expected {}",
                self.version, SCHEMA_VERSION
            ));
        }
        for setting in super::setting_path::GLOBAL_SETTINGS {
            if let Some(value) = self.setting(setting.dotted) {
                validate_value(setting.kind, &value)?;
            }
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

/// Whether a flat setting name is one of the canonical machine-wide names.
///
/// The name is not echoed back by any caller: a name that comes from a
/// file can be an accidentally pasted secret.
pub fn is_known_setting(name: &str) -> bool {
    crate::infra::storage_schema::GLOBAL_SETTING_NAMES.contains(&name)
}

/// Validate a typed value against the kind its path declares.
fn validate_value(kind: SettingKind, value: &SettingValue) -> Result<(), String> {
    match (kind, value) {
        (SettingKind::Text, SettingValue::Text(text)) => {
            if text.trim().is_empty() {
                return Err("value cannot be empty".to_owned());
            }
            Ok(())
        }
        (SettingKind::Bool, SettingValue::Bool(_)) => Ok(()),
        (SettingKind::Port, SettingValue::Port(port)) => {
            if *port == 0 {
                return Err("the port must be greater than zero".to_owned());
            }
            Ok(())
        }
        (SettingKind::List, SettingValue::List(values)) => {
            if values.is_empty() {
                return Err("the list cannot be empty".to_owned());
            }
            if values.iter().any(|value| value.trim().is_empty()) {
                return Err("a list entry cannot be empty".to_owned());
            }
            Ok(())
        }
        (SettingKind::Choice(allowed), SettingValue::Text(text)) => {
            if allowed.contains(&text.as_str()) {
                Ok(())
            } else {
                Err("the value is not one of the allowed literals".to_owned())
            }
        }
        _ => Err("the value does not match the type of this configuration field".to_owned()),
    }
}

/// Parse the flat text form of a value into its typed form.
///
/// The flat text form is the shape the environment overrides and the
/// historical CLI aliases use. The value never appears in an error.
pub fn parse_setting_value(kind: SettingKind, raw: &str) -> Result<SettingValue, String> {
    let trimmed = raw.trim();
    match kind {
        SettingKind::Text => {
            if trimmed.is_empty() {
                return Err("the value cannot be empty".to_owned());
            }
            Ok(SettingValue::Text(trimmed.to_owned()))
        }
        SettingKind::Bool => crate::config_write::parse_bool_literal(trimmed)
            .map(SettingValue::Bool)
            .ok_or_else(|| "invalid boolean value; expected true or false".to_owned()),
        SettingKind::Port => {
            let port: u16 = trimmed
                .parse()
                .map_err(|_| "the port must be a number between 1 and 65535".to_owned())?;
            if port == 0 {
                return Err("the port must be greater than zero".to_owned());
            }
            Ok(SettingValue::Port(port))
        }
        SettingKind::List => {
            let values: Vec<String> = trimmed
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect();
            if values.is_empty() {
                return Err("the list cannot be empty".to_owned());
            }
            Ok(SettingValue::List(values))
        }
        SettingKind::Choice(allowed) => {
            if allowed.contains(&trimmed) {
                Ok(SettingValue::Text(trimmed.to_owned()))
            } else {
                Err(format!(
                    "invalid value; expected one of {}",
                    allowed.join(", ")
                ))
            }
        }
    }
}

/// Read a typed value out of its JSON shape.
fn value_from_json(kind: SettingKind, value: &serde_json::Value) -> Option<SettingValue> {
    match kind {
        SettingKind::Text | SettingKind::Choice(_) => {
            Some(SettingValue::Text(value.as_str()?.to_owned()))
        }
        SettingKind::Bool => Some(SettingValue::Bool(value.as_bool()?)),
        SettingKind::Port => Some(SettingValue::Port(u16::try_from(value.as_u64()?).ok()?)),
        SettingKind::List => Some(SettingValue::List(
            value
                .as_array()?
                .iter()
                .map(|entry| entry.as_str().map(str::to_owned))
                .collect::<Option<Vec<String>>>()?,
        )),
    }
}

/// Write a typed value into the JSON shape at `segments`, creating the
/// missing intermediate objects.
fn set_json_path(
    document: &mut serde_json::Value,
    segments: &[&str],
    value: &SettingValue,
) -> Result<(), String> {
    let (last, parents) = segments
        .split_last()
        .ok_or_else(|| "the configuration path is empty".to_owned())?;
    let mut current = document;
    for segment in parents {
        let object = current
            .as_object_mut()
            .ok_or_else(|| "the configuration path is not an object".to_owned())?;
        let entry = object
            .entry((*segment).to_owned())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if !entry.is_object() {
            *entry = serde_json::Value::Object(serde_json::Map::new());
        }
        current = entry;
    }
    let object = current
        .as_object_mut()
        .ok_or_else(|| "the configuration path is not an object".to_owned())?;
    object.insert((*last).to_owned(), value.to_json());
    Ok(())
}

/// Remove the value at `segments`. Returns `true` when a value was
/// removed.
fn remove_json_path(document: &mut serde_json::Value, segments: &[&str]) -> bool {
    let Some((last, parents)) = segments.split_last() else {
        return false;
    };
    let mut current = document;
    for segment in parents {
        let Some(next) = current.get_mut(*segment) else {
            return false;
        };
        if !next.is_object() {
            return false;
        }
        current = next;
    }
    let Some(object) = current.as_object_mut() else {
        return false;
    };
    object.remove(*last).is_some()
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
