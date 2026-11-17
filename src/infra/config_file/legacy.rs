use super::model::{
    ConfigFile, ForgejoRoleConfig, GitlabRoleConfig, RedmineRoleConfig, RedmineUser,
    RoleLegacyConfig,
};
use crate::policy::Role;
use crate::providers::config::ProviderKind;
use std::collections::BTreeMap;

/// Plain rows mirroring the seven legacy SQLite configuration tables plus the
/// read-only TOML overlay. Callers read the legacy stores; this module only
/// maps values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyConfigRows {
    pub role_configs: Vec<LegacyRoleConfigRow>,
    pub forgejo_configs: Vec<LegacyForgejoConfigRow>,
    pub redmine_configs: Vec<LegacyRedmineConfigRow>,
    pub gitlab_configs: Vec<LegacyGitlabConfigRow>,
    pub credentials: Vec<LegacyCredentialRow>,
    pub redmine_users: Vec<LegacyRedmineUserRow>,
    pub global_settings: Vec<LegacyGlobalSettingRow>,
    pub toml_overlay: Option<LegacyTomlOverlay>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyRoleConfigRow {
    pub role: String,
    pub provider: Option<String>,
    pub api_base: Option<String>,
    pub repository: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyRedmineConfigRow {
    pub role: String,
    pub api_base: Option<String>,
    pub project_id: Option<String>,
    pub close_status_id: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyForgejoConfigRow {
    pub role: String,
    pub api_base: String,
    pub repository: String,
    pub source: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyGitlabConfigRow {
    pub role: String,
    pub api_base: Option<String>,
    pub project_id: Option<i64>,
    pub repository: Option<String>,
    pub source: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyCredentialRow {
    pub role: String,
    pub provider: String,
    pub credential: String,
    pub updated_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyRedmineUserRow {
    pub role: String,
    pub user_id: u64,
    pub login: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyGlobalSettingRow {
    pub name: String,
    pub value: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyTomlOverlay {
    pub default_provider: Option<String>,
    pub redmine_api_base: Option<String>,
    pub redmine_repository_url: Option<String>,
    pub index_backend: Option<String>,
    pub roles: BTreeMap<String, LegacyTomlRole>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyTomlRole {
    pub provider: Option<String>,
    pub redmine_api_base: Option<String>,
    pub redmine_close_status_id: Option<u64>,
}

impl ConfigFile {
    /// Map the seven legacy configuration tables, then fold the explicit TOML
    /// overlay over them with the old TOML-over-SQLite precedence. Ambient
    /// environment variables are never an input.
    ///
    /// Every persisted value is carried over byte for byte. A value the
    /// model cannot represent exactly — a role row whose every column is
    /// NULL, a provider literal that is not the canonical one, a string
    /// the model rejects — is a refusal, never a silent change, and the
    /// refusal happens before any destination file is written.
    pub fn from_legacy(rows: LegacyConfigRows) -> Result<Self, String> {
        let mut file = Self::new();
        apply_global_settings(&mut file, &rows.global_settings)?;
        apply_role_configs(&mut file, &rows.role_configs)?;
        apply_redmine_configs(&mut file, &rows.redmine_configs)?;
        apply_forgejo_configs(&mut file, &rows.forgejo_configs)?;
        apply_gitlab_configs(&mut file, &rows.gitlab_configs)?;
        apply_credentials(&mut file, &rows.credentials)?;
        apply_redmine_users(&mut file, &rows.redmine_users)?;
        // The mapped model is checked against the persisted rows
        // themselves, before the overlay is folded over them: the overlay
        // has the documented precedence and may legitimately replace a
        // persisted value.
        verify_fidelity(&rows, &file)?;
        if let Some(overlay) = rows.toml_overlay {
            apply_toml_overlay(&mut file, overlay)?;
        }
        file.validate()?;
        Ok(file)
    }
}

/// Check that the mapped model still carries every persisted value
/// verbatim.
///
/// The comparison is against the persisted rows, not against the mapping
/// that produced the model, so a normalisation that slipped through is
/// caught here instead of after the source rows were deleted. The values
/// themselves never appear in an error.
fn verify_fidelity(rows: &LegacyConfigRows, file: &ConfigFile) -> Result<(), String> {
    for row in &rows.global_settings {
        if is_ignored_global_setting(&row.name) {
            continue;
        }
        let raw = row.value.as_deref().unwrap_or_default();
        let setting =
            crate::infra::config_file::setting_path::global_setting_by_legacy_name(&row.name)
                .ok_or_else(|| "unknown global setting".to_owned())?;
        let expected = if setting.kind == crate::infra::config_file::SettingKind::Text {
            crate::infra::config_file::SettingValue::Text(raw.to_owned())
        } else {
            typed_global_value(&row.name, raw)?.1
        };
        if file.setting(setting.dotted) != Some(expected) {
            return Err(
                "the migrated configuration does not carry a persisted global setting; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
    }
    for row in &rows.role_configs {
        let entry = file
            .role(role_from_name(&row.role)?)
            .ok_or_else(|| "the migrated configuration lost a role configuration row".to_owned())?;
        if entry.provider.as_deref() != row.provider.as_deref() {
            return Err(
                "the migrated configuration does not carry a persisted role provider verbatim; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
        if entry.legacy.api_base.as_deref() != row.api_base.as_deref() {
            return Err(
                "the migrated configuration does not carry a persisted legacy field verbatim; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
        if entry.legacy.repository.as_deref() != row.repository.as_deref() {
            return Err(
                "the migrated configuration does not carry a persisted legacy field verbatim; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
    }
    for row in &rows.redmine_configs {
        let entry = file
            .role(role_from_name(&row.role)?)
            .ok_or_else(|| "the migrated configuration lost a Redmine config row".to_owned())?;
        let redmine = entry
            .redmine
            .as_ref()
            .ok_or_else(|| "the migrated configuration lost a Redmine config row".to_owned())?;
        if redmine.api_base.as_deref() != row.api_base.as_deref()
            || redmine.project_id.as_deref() != row.project_id.as_deref()
            || redmine.close_status_id != row.close_status_id
        {
            return Err(
                "the migrated configuration does not carry a persisted Redmine field verbatim; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
    }
    for row in &rows.forgejo_configs {
        let entry = file
            .role(role_from_name(&row.role)?)
            .ok_or_else(|| "the migrated configuration lost a Forgejo config row".to_owned())?;
        let forgejo = entry
            .forgejo
            .as_ref()
            .ok_or_else(|| "the migrated configuration lost a Forgejo config row".to_owned())?;
        if forgejo.api_base != row.api_base
            || forgejo.repository != row.repository
            || forgejo.source != row.source
            || forgejo.updated_at != row.updated_at
        {
            return Err(
                "the migrated configuration does not carry a persisted Forgejo field verbatim; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
    }
    for row in &rows.gitlab_configs {
        let entry = file
            .role(role_from_name(&row.role)?)
            .ok_or_else(|| "the migrated configuration lost a GitLab config row".to_owned())?;
        let gitlab = entry
            .gitlab
            .as_ref()
            .ok_or_else(|| "the migrated configuration lost a GitLab config row".to_owned())?;
        if gitlab.api_base.as_deref() != row.api_base.as_deref()
            || gitlab.project_id != row.project_id
            || gitlab.repository.as_deref() != row.repository.as_deref()
            || gitlab.source.as_deref() != row.source.as_deref()
            || gitlab.updated_at != row.updated_at
        {
            return Err(
                "the migrated configuration does not carry a persisted GitLab field verbatim; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
    }
    for row in &rows.credentials {
        let entry = file
            .role(role_from_name(&row.role)?)
            .ok_or_else(|| "the migrated configuration lost a credential row".to_owned())?;
        let credential = entry.credentials.get(&row.provider).ok_or_else(|| {
            "the migrated configuration lost a persisted credential; refusing to migrate it"
                .to_owned()
        })?;
        if credential.value != row.credential || credential.updated_at != row.updated_at {
            return Err(
                "the migrated configuration does not carry a persisted credential verbatim; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
    }
    for row in &rows.redmine_users {
        let entry = file
            .role(role_from_name(&row.role)?)
            .ok_or_else(|| "the migrated configuration lost a Redmine user row".to_owned())?;
        let user = entry
            .redmine_user
            .as_ref()
            .ok_or_else(|| "the migrated configuration lost a Redmine user row".to_owned())?;
        if user.user_id != row.user_id || user.login != row.login {
            return Err(
                "the migrated configuration does not carry a persisted Redmine user verbatim; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn apply_global_settings(
    file: &mut ConfigFile,
    rows: &[LegacyGlobalSettingRow],
) -> Result<(), String> {
    let mut seen = BTreeMap::new();
    // The name of every row is validated first, whatever its value is:
    // an unknown setting must never be skipped, because the purge would
    // delete a persisted value the model cannot represent.
    for row in rows {
        if seen.insert(row.name.clone(), ()).is_some() {
            return Err("duplicate global setting row".to_owned());
        }
        if is_ignored_global_setting(&row.name) {
            // The legacy index-backend selector was always inert: the
            // presence of the PostgreSQL URL selects the backend. It has
            // no destination in the typed tree and is dropped on purpose.
            continue;
        }
        if !crate::infra::config_file::is_known_setting(&row.name) {
            return Err(
                "the source holds a global setting the configuration model does not know; \
                 refusing to migrate it"
                    .to_owned(),
            );
        }
    }
    // A known setting whose persisted value is NULL or blank has no
    // representation: the model refuses an empty value, so the row would
    // be dropped while the purge deleted it. The migration refuses the
    // source instead, before any destination file is written and before
    // anything is purged. The setting name is never echoed back.
    for row in rows {
        let value = row.value.as_deref().unwrap_or_default();
        if value.trim().is_empty() {
            return Err(
                "the source holds a persisted global setting with an empty value that \
                 the configuration model cannot represent; refusing to migrate it"
                    .to_owned(),
            );
        }
    }
    // The value is mapped into its typed destination: a boolean stays a
    // boolean, a port stays a number, a list stays an array. A value the
    // typed tree cannot represent is a refusal, never a silent change.
    for row in rows {
        if is_ignored_global_setting(&row.name) {
            continue;
        }
        let value = row.value.as_deref().unwrap_or_default();
        let (path, typed) = typed_global_value(&row.name, value)?;
        file.set_setting(path, typed)?;
    }
    Ok(())
}

/// The legacy settings that have no destination in the typed tree.
///
/// The index-backend selector was inert: the presence of the PostgreSQL
/// URL selects the backend, so the persisted value never changed anything.
fn is_ignored_global_setting(name: &str) -> bool {
    name == "PHASEGENT_INDEX_BACKEND"
}

/// Map a persisted flat setting onto its typed destination.
///
/// The value is converted into the type the destination declares, so a
/// persisted `"1"` becomes the boolean `true` and a persisted
/// `"587,25"` becomes the list `["587", "25"]`. A value the typed tree
/// cannot represent is refused. The value is never echoed back.
fn typed_global_value(
    name: &str,
    raw: &str,
) -> Result<(&'static str, crate::infra::config_file::SettingValue), String> {
    let Some(setting) =
        crate::infra::config_file::setting_path::global_setting_by_legacy_name(name)
    else {
        return Err(
            "the source holds a global setting the configuration model does not know; \
             refusing to migrate it"
                .to_owned(),
        );
    };
    use crate::infra::config_file::{SettingKind, SettingValue, parse_setting_value};
    let value = match setting.kind {
        SettingKind::Text if !raw.trim().is_empty() => Ok(SettingValue::Text(raw.to_owned())),
        SettingKind::Choice(_)
            if matches!(
                name,
                "PHASEGENT_NOTIFY_CHANNEL"
                    | "PHASEGENT_NOTIFY_EMAIL_TLS"
                    | "PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE"
            ) =>
        {
            parse_setting_value(setting.kind, &raw.trim().to_ascii_lowercase())
        }
        _ => parse_setting_value(setting.kind, raw),
    }
    .map_err(|_| {
        "the source holds a global setting value the typed configuration cannot represent; \
         refusing to migrate it"
            .to_owned()
    })?;
    Ok((setting.dotted, value))
}

fn apply_role_configs(file: &mut ConfigFile, rows: &[LegacyRoleConfigRow]) -> Result<(), String> {
    let mut seen = BTreeMap::new();
    for row in rows {
        if seen.insert(row.role.clone(), ()).is_some() {
            return Err("duplicate role config row".to_owned());
        }
        let role = role_from_name(&row.role)?;
        // A row whose every column is NULL cannot be told apart from a
        // missing row by the configuration model, so it is refused
        // instead of being migrated as an absent role.
        if row.provider.is_none() && row.api_base.is_none() && row.repository.is_none() {
            return Err(
                "the source holds a role configuration row whose every column is NULL; \
                 the configuration model cannot represent it; refusing to migrate it"
                    .to_owned(),
            );
        }
        if let Some(provider) = row.provider.as_deref() {
            // The model stores the canonical provider literal, so a
            // persisted value that is not exactly that literal cannot be
            // carried over byte for byte.
            canonical_provider(provider)?;
            file.set_role_provider(role, Some(provider))?;
        }
        // The inert legacy columns keep the bytes they were persisted
        // with, including an empty string, which is a persisted value
        // and not the same as an absent one.
        let legacy = RoleLegacyConfig {
            api_base: row.api_base.clone(),
            repository: row.repository.clone(),
        };
        if !legacy.is_empty() {
            file.role_config_mut(role).legacy = legacy;
        }
    }
    Ok(())
}

/// The canonical provider literal of a persisted provider value.
///
/// The model stores the canonical literal, so a persisted value that is
/// not exactly that literal cannot be carried over byte for byte. The
/// value is never echoed back.
fn canonical_provider(value: &str) -> Result<&'static str, String> {
    let kind = value.parse::<ProviderKind>().map_err(|_| {
        "the source holds a role provider the configuration model cannot represent; \
         refusing to migrate it"
            .to_owned()
    })?;
    Ok(kind.as_str())
}

fn apply_redmine_configs(
    file: &mut ConfigFile,
    rows: &[LegacyRedmineConfigRow],
) -> Result<(), String> {
    let mut seen = BTreeMap::new();
    for row in rows {
        if seen.insert(row.role.clone(), ()).is_some() {
            return Err("duplicate Redmine config row".to_owned());
        }
        let role = role_from_name(&row.role)?;
        // The row is carried over as it was persisted. The model keeps the
        // row itself distinguishable from a missing one, and every string
        // keeps its bytes.
        file.set_role_redmine_config(
            role,
            RedmineRoleConfig {
                api_base: row.api_base.clone(),
                project_id: row.project_id.clone(),
                close_status_id: row.close_status_id,
            },
        )?;
    }
    Ok(())
}

fn apply_forgejo_configs(
    file: &mut ConfigFile,
    rows: &[LegacyForgejoConfigRow],
) -> Result<(), String> {
    let mut seen = BTreeMap::new();
    for row in rows {
        if seen.insert(row.role.clone(), ()).is_some() {
            return Err("duplicate Forgejo config row".to_owned());
        }
        let role = role_from_name(&row.role)?;
        file.set_role_forgejo_config(
            role,
            ForgejoRoleConfig {
                api_base: row.api_base.clone(),
                repository: row.repository.clone(),
                source: row.source.clone(),
                updated_at: row.updated_at,
            },
        )?;
    }
    Ok(())
}

fn apply_gitlab_configs(
    file: &mut ConfigFile,
    rows: &[LegacyGitlabConfigRow],
) -> Result<(), String> {
    let mut seen = BTreeMap::new();
    for row in rows {
        if seen.insert(row.role.clone(), ()).is_some() {
            return Err("duplicate GitLab config row".to_owned());
        }
        let role = role_from_name(&row.role)?;
        // The strings keep their bytes. A value the model rejects — an
        // empty GitLab address, for example — is a refusal, never a
        // silently dropped field.
        file.set_role_gitlab_config(
            role,
            GitlabRoleConfig {
                api_base: row.api_base.clone(),
                project_id: row.project_id,
                repository: row.repository.clone(),
                source: row.source.clone(),
                updated_at: row.updated_at,
            },
        )?;
    }
    Ok(())
}

fn apply_credentials(file: &mut ConfigFile, rows: &[LegacyCredentialRow]) -> Result<(), String> {
    let mut seen = BTreeMap::new();
    for row in rows {
        if seen
            .insert((row.role.clone(), row.provider.clone()), ())
            .is_some()
        {
            return Err("duplicate credential row".to_owned());
        }
        let role = role_from_name(&row.role)?;
        // The secret and its timestamp are carried over exactly as they
        // were persisted. The secret is never echoed back.
        file.set_credential_raw(role, &row.provider, &row.credential, row.updated_at)?;
    }
    Ok(())
}

fn apply_redmine_users(file: &mut ConfigFile, rows: &[LegacyRedmineUserRow]) -> Result<(), String> {
    let mut seen = BTreeMap::new();
    for row in rows {
        if seen.insert(row.role.clone(), ()).is_some() {
            return Err("duplicate Redmine user row".to_owned());
        }
        let role = role_from_name(&row.role)?;
        // The login keeps the bytes it was persisted with.
        file.set_role_redmine_user(
            role,
            RedmineUser {
                user_id: row.user_id,
                login: row.login.clone(),
            },
        )?;
    }
    Ok(())
}

fn apply_toml_overlay(file: &mut ConfigFile, overlay: LegacyTomlOverlay) -> Result<(), String> {
    // The overlay decoder already trimmed and validated every value with
    // the documented legacy semantics, so the values are applied exactly
    // as they were decoded. A value that is present but empty would be a
    // silent omission, so it is refused instead.
    if let Some(value) = overlay.default_provider.as_deref() {
        let provider = value
            .parse::<ProviderKind>()
            .map_err(|_| "TOML default_provider is invalid".to_owned())?;
        file.set_setting(
            crate::infra::config_file::setting_path::PROVIDER,
            crate::infra::config_file::SettingValue::Text(provider.as_str().to_owned()),
        )?;
    }
    if let Some(value) = overlay.redmine_api_base.as_deref() {
        refuse_empty_overlay_value(value, "redmine_api_base")?;
        file.set_setting(
            crate::infra::config_file::setting_path::REDMINE_API_BASE,
            crate::infra::config_file::SettingValue::Text(value.to_owned()),
        )?;
    }
    if let Some(value) = overlay.redmine_repository_url.as_deref() {
        refuse_empty_overlay_value(value, "redmine_repository_url")?;
        file.set_setting(
            crate::infra::config_file::setting_path::REDMINE_REPOSITORY_URL,
            crate::infra::config_file::SettingValue::Text(value.to_owned()),
        )?;
    }
    if let Some(value) = overlay.index_backend.as_deref() {
        refuse_empty_overlay_value(value, "index_backend")?;
        // The legacy backend selector was inert: the presence of the
        // PostgreSQL URL selects the backend. It has no destination in the
        // typed tree, so the overlay value is dropped on purpose.
    }
    for (name, role_overlay) in overlay.roles {
        let role = role_from_name(&name)?;
        if let Some(value) = role_overlay.provider.as_deref() {
            refuse_empty_overlay_value(value, "provider")?;
            file.set_role_provider(role, Some(value))?;
        }
        if let Some(value) = role_overlay.redmine_api_base.as_deref() {
            refuse_empty_overlay_value(value, "redmine_api_base")?;
            let entry = file.role_config_mut(role);
            entry
                .redmine
                .get_or_insert_with(RedmineRoleConfig::default)
                .api_base = Some(value.to_owned());
        }
        if let Some(value) = role_overlay.redmine_close_status_id {
            let entry = file.role_config_mut(role);
            entry
                .redmine
                .get_or_insert_with(RedmineRoleConfig::default)
                .close_status_id = Some(value);
        }
    }
    Ok(())
}

/// Refuse an overlay value that carries nothing, instead of skipping it.
///
/// The overlay is explicit operator input, so a value it defines is never
/// dropped silently. The value itself is never echoed back.
fn refuse_empty_overlay_value(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("the TOML overlay field '{field}' cannot be empty"));
    }
    Ok(())
}

fn role_from_name(name: &str) -> Result<Role, String> {
    match name {
        "admin" => Ok(Role::Admin),
        "orchestrator" => Ok(Role::Orchestrator),
        "executor" => Ok(Role::Executor),
        "reviewer" => Ok(Role::Reviewer),
        "explore" => Ok(Role::Explore),
        // The role name comes from the source database and is never echoed
        // back: it is untrusted input.
        _ => Err("unknown role".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::config_file::SettingValue;

    fn complete_rows() -> LegacyConfigRows {
        LegacyConfigRows {
            role_configs: vec![LegacyRoleConfigRow {
                role: "executor".to_owned(),
                provider: Some("forgejo".to_owned()),
                api_base: Some("https://legacy-role.example".to_owned()),
                repository: Some("legacy/repo".to_owned()),
            }],
            redmine_configs: vec![LegacyRedmineConfigRow {
                role: "executor".to_owned(),
                api_base: Some("https://legacy-redmine.example".to_owned()),
                project_id: Some("tools".to_owned()),
                close_status_id: Some(5),
            }],
            forgejo_configs: vec![LegacyForgejoConfigRow {
                role: "executor".to_owned(),
                api_base: "https://forgejo.example".to_owned(),
                repository: "owner/repo".to_owned(),
                source: "admin-auth-setup".to_owned(),
                updated_at: 1_700_000_000,
            }],
            gitlab_configs: vec![LegacyGitlabConfigRow {
                role: "reviewer".to_owned(),
                api_base: Some("https://gitlab.example".to_owned()),
                project_id: Some(42),
                repository: Some("owner/repo".to_owned()),
                source: Some("admin-auth-setup".to_owned()),
                updated_at: Some(1_700_000_001),
            }],
            credentials: vec![
                LegacyCredentialRow {
                    role: "executor".to_owned(),
                    provider: "forgejo".to_owned(),
                    credential: "forgejo-secret".to_owned(),
                    updated_at: Some(1_700_000_000),
                },
                LegacyCredentialRow {
                    role: "reviewer".to_owned(),
                    provider: "gitlab".to_owned(),
                    credential: "gitlab-secret".to_owned(),
                    updated_at: None,
                },
            ],
            redmine_users: vec![LegacyRedmineUserRow {
                role: "executor".to_owned(),
                user_id: 7,
                login: "phasegent-executor".to_owned(),
            }],
            global_settings: vec![
                LegacyGlobalSettingRow {
                    name: "PHASEGENT_DEFAULT_PROVIDER".to_owned(),
                    value: Some("redmine".to_owned()),
                },
                LegacyGlobalSettingRow {
                    name: "PHASEGENT_INDEX_BACKEND".to_owned(),
                    value: Some("sqlite".to_owned()),
                },
            ],
            toml_overlay: None,
        }
    }

    #[test]
    fn all_seven_legacy_tables_map_into_the_typed_model() {
        let file = ConfigFile::from_legacy(complete_rows()).unwrap();

        assert_eq!(
            file.version,
            crate::infra::config_file::SCHEMA_VERSION,
            "the importer writes the current schema version"
        );
        assert_eq!(
            file.setting("provider"),
            Some(SettingValue::Text("redmine".to_owned()))
        );
        assert_eq!(
            file.setting("index.postgres.url"),
            None,
            "the inert index backend selector has no destination"
        );
        assert_eq!(file.setting("notifications.ntfy.topic"), None);

        let executor = file.role(Role::Executor).unwrap();
        assert_eq!(executor.provider.as_deref(), Some("forgejo"));
        assert_eq!(
            executor.legacy.api_base.as_deref(),
            Some("https://legacy-role.example")
        );
        assert_eq!(executor.legacy.repository.as_deref(), Some("legacy/repo"));
        let redmine = executor.redmine.as_ref().unwrap();
        assert_eq!(
            redmine.api_base.as_deref(),
            Some("https://legacy-redmine.example")
        );
        assert_eq!(redmine.project_id.as_deref(), Some("tools"));
        assert_eq!(redmine.close_status_id, Some(5));
        let forgejo = executor.forgejo.as_ref().unwrap();
        assert_eq!(forgejo.api_base, "https://forgejo.example");
        assert_eq!(forgejo.repository, "owner/repo");
        assert_eq!(forgejo.source, "admin-auth-setup");
        assert_eq!(forgejo.updated_at, 1_700_000_000);
        assert_eq!(
            executor
                .credentials
                .get("forgejo")
                .map(|entry| (entry.value.as_str(), entry.updated_at)),
            Some(("forgejo-secret", Some(1_700_000_000)))
        );
        assert_eq!(
            executor.redmine_user.as_ref().map(|user| user.user_id),
            Some(7)
        );

        let reviewer = file.role(Role::Reviewer).unwrap();
        let gitlab = reviewer.gitlab.as_ref().unwrap();
        assert_eq!(gitlab.api_base.as_deref(), Some("https://gitlab.example"));
        assert_eq!(gitlab.project_id, Some(42));
        assert_eq!(gitlab.updated_at, Some(1_700_000_001));
        assert_eq!(
            reviewer
                .credentials
                .get("gitlab")
                .map(|entry| (entry.value.as_str(), entry.updated_at)),
            Some(("gitlab-secret", None))
        );
    }

    #[test]
    fn toml_overlay_keeps_its_old_precedence_over_sqlite() {
        let mut rows = complete_rows();
        rows.toml_overlay = Some(LegacyTomlOverlay {
            default_provider: Some("forgejo".to_owned()),
            redmine_api_base: Some("https://toml-redmine.example".to_owned()),
            redmine_repository_url: Some("ssh://git@example/owner/repo.git".to_owned()),
            index_backend: Some("POSTGRES".to_owned()),
            roles: BTreeMap::from([(
                "executor".to_owned(),
                LegacyTomlRole {
                    provider: Some("gitlab".to_owned()),
                    redmine_api_base: Some("https://toml-role-redmine.example".to_owned()),
                    redmine_close_status_id: Some(9),
                },
            )]),
        });

        let file = ConfigFile::from_legacy(rows).unwrap();

        assert_eq!(
            file.setting("provider"),
            Some(SettingValue::Text("forgejo".to_owned()))
        );
        assert_eq!(
            file.setting("redmine.apiBase"),
            Some(SettingValue::Text(
                "https://toml-redmine.example".to_owned()
            ))
        );
        assert_eq!(
            file.setting("redmine.repositoryUrl"),
            Some(SettingValue::Text(
                "ssh://git@example/owner/repo.git".to_owned()
            ))
        );
        assert_eq!(
            file.setting("index.postgres.url"),
            None,
            "the inert index backend selector has no destination"
        );

        let executor = file.role(Role::Executor).unwrap();
        assert_eq!(executor.provider.as_deref(), Some("gitlab"));
        let redmine = executor.redmine.as_ref().unwrap();
        assert_eq!(redmine.close_status_id, Some(9));
        assert_eq!(
            redmine.api_base.as_deref(),
            Some("https://toml-role-redmine.example")
        );
    }

    #[test]
    fn duplicate_and_unknown_legacy_rows_are_rejected() {
        let mut rows = complete_rows();
        rows.credentials.push(LegacyCredentialRow {
            role: "executor".to_owned(),
            provider: "forgejo".to_owned(),
            credential: "other-secret".to_owned(),
            updated_at: None,
        });
        let error = ConfigFile::from_legacy(rows).unwrap_err();
        assert!(error.contains("duplicate credential row"));
        assert!(!error.contains("forgejo-secret"));
        assert!(!error.contains("other-secret"));

        let mut rows = complete_rows();
        rows.role_configs[0].role = "intruder".to_owned();
        assert!(ConfigFile::from_legacy(rows).is_err());

        let mut rows = complete_rows();
        rows.global_settings.push(LegacyGlobalSettingRow {
            name: "PHASEGENT_UNKNOWN".to_owned(),
            value: Some("x".to_owned()),
        });
        assert!(ConfigFile::from_legacy(rows).is_err());
    }

    #[test]
    fn an_unknown_global_setting_is_refused_even_with_an_empty_value() {
        for value in [None, Some(String::new()), Some("  ".to_owned())] {
            let mut rows = complete_rows();
            rows.global_settings.push(LegacyGlobalSettingRow {
                name: "UNRECOGNIZED_SETTING".to_owned(),
                value,
            });
            let error =
                ConfigFile::from_legacy(rows).expect_err("an unknown setting must be refused");
            assert!(error.contains("does not know"), "got: {error}");
            assert!(
                !error.contains("UNRECOGNIZED_SETTING"),
                "the untrusted name leaked: {error}"
            );
        }
    }

    #[test]
    fn a_known_global_setting_with_an_empty_value_is_refused() {
        for value in [None, Some(String::new()), Some("  ".to_owned())] {
            let mut rows = complete_rows();
            rows.global_settings.push(LegacyGlobalSettingRow {
                name: "PHASEGENT_REDMINE_REPOSITORY_URL".to_owned(),
                value,
            });
            let error = ConfigFile::from_legacy(rows)
                .expect_err("an empty persisted setting must be refused, not dropped");
            assert!(error.contains("empty value"), "got: {error}");
        }
    }

    #[test]
    fn an_unknown_setting_name_is_refused_before_an_empty_value_is_inspected() {
        let mut rows = complete_rows();
        rows.global_settings.push(LegacyGlobalSettingRow {
            name: "PHASEGENT_REDMINE_REPOSITORY_URL".to_owned(),
            value: None,
        });
        rows.global_settings.push(LegacyGlobalSettingRow {
            name: "SYNTHETIC_UNKNOWN_SETTING".to_owned(),
            value: Some(String::new()),
        });
        let error = ConfigFile::from_legacy(rows).expect_err("an unknown setting must be refused");
        assert!(error.contains("does not know"), "got: {error}");
        assert!(
            !error.contains("SYNTHETIC_UNKNOWN_SETTING"),
            "the untrusted name leaked: {error}"
        );
    }

    #[test]
    fn a_persisted_role_field_keeps_its_bytes() {
        let mut rows = complete_rows();
        rows.role_configs[0].api_base = Some("  ".to_owned());
        rows.role_configs[0].repository = Some(String::new());
        rows.redmine_configs[0].api_base = Some("   ".to_owned());
        rows.redmine_configs[0].project_id = Some(" legacy-project ".to_owned());
        rows.gitlab_configs[0].api_base = Some("  https://gitlab.example  ".to_owned());
        rows.gitlab_configs[0].repository = Some(" owner/repo ".to_owned());

        let file = ConfigFile::from_legacy(rows).expect("the raw fields must map");

        let executor = file.role(Role::Executor).unwrap();
        assert_eq!(executor.legacy.api_base.as_deref(), Some("  "));
        assert_eq!(executor.legacy.repository.as_deref(), Some(""));
        let redmine = executor.redmine.as_ref().unwrap();
        assert_eq!(redmine.api_base.as_deref(), Some("   "));
        assert_eq!(redmine.project_id.as_deref(), Some(" legacy-project "));
        let reviewer = file.role(Role::Reviewer).unwrap();
        let gitlab = reviewer.gitlab.as_ref().unwrap();
        assert_eq!(
            gitlab.api_base.as_deref(),
            Some("  https://gitlab.example  ")
        );
        assert_eq!(gitlab.repository.as_deref(), Some(" owner/repo "));
    }

    #[test]
    fn a_persisted_global_setting_maps_onto_its_typed_destination() {
        let mut rows = complete_rows();
        rows.global_settings[0].value = Some(" redmine ".to_owned());
        rows.global_settings.push(LegacyGlobalSettingRow {
            name: "PHASEGENT_NOTIFY_NTFY_TOPIC".to_owned(),
            value: Some(" topic ".to_owned()),
        });

        let file = ConfigFile::from_legacy(rows).expect("the raw value must map");

        assert_eq!(
            file.setting("provider"),
            Some(SettingValue::Text("redmine".to_owned())),
            "the typed mapping drops the surrounding whitespace"
        );
        assert_eq!(
            file.setting("notifications.ntfy.topic"),
            Some(SettingValue::Text(" topic ".to_owned()))
        );
    }

    #[test]
    fn legacy_choices_are_case_insensitive_and_text_fidelity_is_checked_against_source() {
        let mut rows = complete_rows();
        for (name, value) in [
            ("PHASEGENT_NOTIFY_CHANNEL", "EMAIL"),
            ("PHASEGENT_NOTIFY_EMAIL_TLS", "STARTTLS"),
            ("PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE", "MARKDOWN"),
            ("PHASEGENT_INDEX_PG_URL", "  postgres://example/db  "),
        ] {
            rows.global_settings.push(LegacyGlobalSettingRow {
                name: name.into(),
                value: Some(value.into()),
            });
        }
        let mut file = ConfigFile::from_legacy(rows.clone()).unwrap();
        assert_eq!(
            file.setting("notifications.channel"),
            Some(SettingValue::Text("email".into()))
        );
        assert_eq!(
            file.setting("notifications.email.smtp.tls"),
            Some(SettingValue::Text("starttls".into()))
        );
        assert_eq!(
            file.setting("notifications.dingtalk.messageType"),
            Some(SettingValue::Text("markdown".into()))
        );
        file.set_setting(
            "index.postgres.url",
            SettingValue::Text("postgres://example/db".into()),
        )
        .unwrap();
        assert!(verify_fidelity(&rows, &file).is_err());
    }

    #[test]
    fn legacy_global_provider_remains_case_sensitive() {
        let mut rows = complete_rows();
        rows.global_settings[0].value = Some("REDMINE".into());
        assert!(ConfigFile::from_legacy(rows).is_err());
        assert!(typed_global_value("PHASEGENT_DEFAULT_PROVIDER", "REDMINE").is_err());
        assert_eq!(
            typed_global_value("PHASEGENT_DEFAULT_PROVIDER", " redmine ")
                .unwrap()
                .1,
            SettingValue::Text("redmine".into())
        );
    }

    #[test]
    fn an_all_null_role_row_is_refused() {
        let mut rows = complete_rows();
        rows.role_configs[0].provider = None;
        rows.role_configs[0].api_base = None;
        rows.role_configs[0].repository = None;
        let error = ConfigFile::from_legacy(rows)
            .expect_err("a role row that carries nothing cannot be represented");
        assert!(error.contains("every column is NULL"), "got: {error}");
    }

    #[test]
    fn a_role_provider_that_is_not_canonical_is_refused_without_echoing_it() {
        for provider in [" forgejo ", "Forgejo", "bitbucket"] {
            let mut rows = complete_rows();
            rows.role_configs[0].provider = Some(provider.to_owned());
            let error = ConfigFile::from_legacy(rows)
                .expect_err("a provider the model cannot carry must be refused");
            assert!(
                error.contains("role provider"),
                "expected a provider refusal, got: {error}"
            );
            assert!(
                !error.contains(provider),
                "the untrusted value leaked: {error}"
            );
        }
    }

    #[test]
    fn a_persisted_credential_keeps_its_bytes_and_is_never_echoed() {
        let mut rows = complete_rows();
        rows.credentials[0].credential = " forgejo-secret ".to_owned();
        let file = ConfigFile::from_legacy(rows).expect("the raw secret must map");
        assert_eq!(
            file.role(Role::Executor)
                .unwrap()
                .credentials
                .get("forgejo")
                .map(|entry| entry.value.as_str()),
            Some(" forgejo-secret ")
        );

        let mut rows = complete_rows();
        rows.credentials[0].credential = "   ".to_owned();
        let error = ConfigFile::from_legacy(rows)
            .expect_err("a secret that carries nothing must be refused");
        assert!(error.contains("cannot be empty"), "got: {error}");
        assert!(!error.contains("   "), "the secret leaked: {error}");
    }

    #[test]
    fn a_persisted_redmine_login_keeps_its_bytes() {
        let mut rows = complete_rows();
        rows.redmine_users[0].login = " phasegent-executor ".to_owned();
        let file = ConfigFile::from_legacy(rows).expect("the raw login must map");
        assert_eq!(
            file.role(Role::Executor)
                .unwrap()
                .redmine_user
                .as_ref()
                .map(|user| user.login.as_str()),
            Some(" phasegent-executor ")
        );

        let mut rows = complete_rows();
        rows.redmine_users[0].login = "   ".to_owned();
        assert!(
            ConfigFile::from_legacy(rows).is_err(),
            "a login that carries nothing must be refused"
        );
    }

    #[test]
    fn migration_does_not_echo_credential_values() {
        let mut rows = complete_rows();
        rows.credentials[0].credential = "  ".to_owned();
        let error = ConfigFile::from_legacy(rows).unwrap_err();
        assert!(!error.contains("forgejo-secret"));
        assert!(!error.contains("  "));
    }

    #[test]
    fn empty_rows_produce_an_empty_document() {
        let file = ConfigFile::from_legacy(LegacyConfigRows::default()).unwrap();
        assert_eq!(file, ConfigFile::new());
        assert!(file.roles.is_empty());
    }
}
