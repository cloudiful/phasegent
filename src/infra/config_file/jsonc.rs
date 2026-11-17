use super::model::{
    ConfigFile, ForgejoRoleConfig, GitlabRoleConfig, RedmineRoleConfig, RedmineUser,
    RoleLegacyConfig,
};
use crate::policy::Role;
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use std::collections::HashSet;

/// Strict JSONC profile: comments and trailing commas only. Every other
/// JSON5 extension the parser understands stays disabled.
pub fn parse_options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}

/// A parsed configuration document that keeps the original text so setting
/// changes can be applied through the CST without dropping comments or
/// unrelated fields.
#[derive(Clone, Debug)]
pub struct ConfigDocument {
    text: String,
    model: ConfigFile,
}

impl ConfigDocument {
    pub fn new() -> Self {
        Self {
            text: format!(
                "{{\n  \"version\": {},\n  \"settings\": {{}},\n  \"roles\": {{}}\n}}\n",
                crate::infra::config_file::SCHEMA_VERSION
            ),
            model: ConfigFile::new(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let model = parse_model(text)?;
        Ok(Self {
            text: text.to_owned(),
            model,
        })
    }

    pub fn model(&self) -> &ConfigFile {
        &self.model
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn set_setting(&mut self, name: &str, value: &str) -> Result<(), String> {
        let mut model = self.model.clone();
        model.set_setting(name, value)?;
        let text = set_setting_text(
            &self.text,
            name,
            model.get_setting(name).unwrap_or_default(),
        )?;
        self.replace(text, model)
    }

    pub fn clear_setting(&mut self, name: &str) -> Result<bool, String> {
        let mut model = self.model.clone();
        let removed = model.clear_setting(name);
        if !removed {
            return Ok(false);
        }
        let text = clear_setting_text(&self.text, name)?;
        self.replace(text, model)?;
        Ok(true)
    }

    pub fn set_role_provider(&mut self, role: Role, provider: Option<&str>) -> Result<(), String> {
        let mut model = self.model.clone();
        model.set_role_provider(role, provider)?;
        let text = set_role_provider_text(&self.text, role, model.provider(role))?;
        self.replace(text, model)
    }

    pub fn set_credential(
        &mut self,
        role: Role,
        provider: &str,
        value: &str,
        updated_at: Option<i64>,
    ) -> Result<(), String> {
        let mut model = self.model.clone();
        model.set_credential(role, provider, value, updated_at)?;
        let entry = model
            .credential(role, provider)
            .expect("credential was just validated");
        let text = set_credential_text(&self.text, role, provider, &entry.value, entry.updated_at)?;
        self.replace(text, model)
    }

    /// Replace the role-scoped Redmine configuration.
    pub fn set_role_redmine_config(
        &mut self,
        role: Role,
        config: RedmineRoleConfig,
    ) -> Result<(), String> {
        let mut model = self.model.clone();
        model.set_role_redmine_config(role, config.clone())?;
        let text = set_role_redmine_text(&self.text, role, &config)?;
        self.replace(text, model)
    }

    /// Replace the role-scoped Forgejo configuration.
    pub fn set_role_forgejo_config(
        &mut self,
        role: Role,
        config: ForgejoRoleConfig,
    ) -> Result<(), String> {
        let mut model = self.model.clone();
        model.set_role_forgejo_config(role, config.clone())?;
        let text = set_role_forgejo_text(&self.text, role, &config)?;
        self.replace(text, model)
    }

    /// Replace the role-scoped GitLab configuration.
    pub fn set_role_gitlab_config(
        &mut self,
        role: Role,
        config: GitlabRoleConfig,
    ) -> Result<(), String> {
        let mut model = self.model.clone();
        model.set_role_gitlab_config(role, config.clone())?;
        let text = set_role_gitlab_text(&self.text, role, &config)?;
        self.replace(text, model)
    }

    /// Replace the admin-provisioned Redmine identity of a role.
    pub fn set_role_redmine_user(&mut self, role: Role, user: RedmineUser) -> Result<(), String> {
        let mut model = self.model.clone();
        model.set_role_redmine_user(role, user.clone())?;
        let text = set_role_redmine_user_text(&self.text, role, &user)?;
        self.replace(text, model)
    }

    /// Replace the inert legacy `api_base`/`repository` columns of a role.
    pub fn set_role_legacy(&mut self, role: Role, legacy: RoleLegacyConfig) -> Result<(), String> {
        let mut model = self.model.clone();
        model.set_role_legacy(role, legacy.clone());
        let text = set_role_legacy_text(&self.text, role, &legacy)?;
        self.replace(text, model)
    }

    fn replace(&mut self, text: String, expected: ConfigFile) -> Result<(), String> {
        let document = Self::parse(&text)?;
        if document.model != expected {
            return Err("configuration update could not be applied consistently".to_owned());
        }
        self.text = text;
        self.model = document.model;
        Ok(())
    }
}

impl Default for ConfigDocument {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ConfigDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.text)
    }
}

pub fn parse_model(text: &str) -> Result<ConfigFile, String> {
    let root = parse_root(text)?;
    let Some(root_value) = root.value() else {
        return Err("JSONC configuration must contain a top-level object".to_owned());
    };
    if root_value.as_object().is_none() {
        return Err("JSONC configuration must contain a top-level object".to_owned());
    }
    check_duplicate_keys(&root_value)?;
    let value = root
        .to_serde_value()
        .ok_or_else(|| "could not convert JSONC configuration".to_owned())?;
    let model: ConfigFile =
        serde_json::from_value(value).map_err(|error| safe_deserialize_error(&error))?;
    model.validate()?;
    Ok(model)
}

fn parse_root(text: &str) -> Result<CstRootNode, String> {
    CstRootNode::parse(text, &parse_options()).map_err(|error| {
        format!(
            "could not parse JSONC configuration at line {} column {}: {}",
            error.line_display(),
            error.column_display(),
            error.kind()
        )
    })
}

fn safe_deserialize_error(error: &serde_json::Error) -> String {
    let reason = match error.classify() {
        serde_json::error::Category::Io => "read error",
        serde_json::error::Category::Syntax => "invalid structure",
        serde_json::error::Category::Data => "invalid field name, type, or value",
        serde_json::error::Category::Eof => "unexpected end of input",
    };
    format!(
        "invalid JSONC configuration: {reason} at line {} column {}",
        error.line(),
        error.column()
    )
}

fn check_duplicate_keys(node: &jsonc_parser::cst::CstNode) -> Result<(), String> {
    if let Some(object) = node.as_object() {
        check_object_duplicates(&object)?;
    } else if let Some(array) = node.as_array() {
        for child in array.children_exclude_trivia_and_tokens() {
            check_duplicate_keys(&child)?;
        }
    }
    Ok(())
}

fn check_object_duplicates(object: &CstObject) -> Result<(), String> {
    let mut seen = HashSet::new();
    for property in object.properties() {
        let name = property
            .decoded_name()
            .ok_or_else(|| "invalid JSONC object property name".to_owned())?;
        if !seen.insert(name) {
            return Err("duplicate object key in JSONC configuration".to_owned());
        }
        if let Some(value) = property.value() {
            check_duplicate_keys(&value)?;
        }
    }
    Ok(())
}

fn set_setting_text(text: &str, name: &str, value: &str) -> Result<String, String> {
    let root = parse_root(text)?;
    let root_object = root.object_value_or_set();
    let settings = root_object.object_value_or_set("settings");
    set_string_property(&settings, name, value);
    Ok(root.to_string())
}

fn clear_setting_text(text: &str, name: &str) -> Result<String, String> {
    let root = parse_root(text)?;
    let root_object = root.object_value_or_set();
    let settings = root_object.object_value_or_set("settings");
    remove_property(&settings, name);
    Ok(root.to_string())
}

fn set_role_provider_text(
    text: &str,
    role: Role,
    provider: Option<&str>,
) -> Result<String, String> {
    let root = parse_root(text)?;
    let roles = root.object_value_or_set().object_value_or_set("roles");
    let role_object = roles.object_value_or_set(role.as_str());
    match provider {
        Some(provider) => set_string_property(&role_object, "provider", provider),
        None => remove_property(&role_object, "provider"),
    }
    Ok(root.to_string())
}

fn set_credential_text(
    text: &str,
    role: Role,
    provider: &str,
    value: &str,
    updated_at: Option<i64>,
) -> Result<String, String> {
    let root = parse_root(text)?;
    let roles = root.object_value_or_set().object_value_or_set("roles");
    let role_object = roles.object_value_or_set(role.as_str());
    let credentials = role_object.object_value_or_set("credentials");
    let entry = credentials.object_value_or_set(provider);
    set_string_property(&entry, "value", value);
    match updated_at {
        Some(updated_at) => set_number_property(&entry, "updatedAt", updated_at),
        None => remove_property(&entry, "updatedAt"),
    }
    Ok(root.to_string())
}

fn set_role_redmine_text(
    text: &str,
    role: Role,
    config: &RedmineRoleConfig,
) -> Result<String, String> {
    let root = parse_root(text)?;
    let redmine = role_entry_object(&root, role).object_value_or_set("redmine");
    set_optional_string_property(&redmine, "apiBase", config.api_base.as_deref());
    set_optional_string_property(&redmine, "projectId", config.project_id.as_deref());
    match config.close_status_id {
        Some(close_status_id) => {
            set_number_property(&redmine, "closeStatusId", close_status_id as i64)
        }
        None => remove_property(&redmine, "closeStatusId"),
    }
    Ok(root.to_string())
}

fn set_role_forgejo_text(
    text: &str,
    role: Role,
    config: &ForgejoRoleConfig,
) -> Result<String, String> {
    let root = parse_root(text)?;
    let forgejo = role_entry_object(&root, role).object_value_or_set("forgejo");
    set_string_property(&forgejo, "apiBase", &config.api_base);
    set_string_property(&forgejo, "repository", &config.repository);
    set_string_property(&forgejo, "source", &config.source);
    set_number_property(&forgejo, "updatedAt", config.updated_at);
    Ok(root.to_string())
}

fn set_role_gitlab_text(
    text: &str,
    role: Role,
    config: &GitlabRoleConfig,
) -> Result<String, String> {
    let root = parse_root(text)?;
    let gitlab = role_entry_object(&root, role).object_value_or_set("gitlab");
    set_optional_string_property(&gitlab, "apiBase", config.api_base.as_deref());
    match config.project_id {
        Some(project_id) => set_number_property(&gitlab, "projectId", project_id),
        None => remove_property(&gitlab, "projectId"),
    }
    set_optional_string_property(&gitlab, "repository", config.repository.as_deref());
    set_optional_string_property(&gitlab, "source", config.source.as_deref());
    match config.updated_at {
        Some(updated_at) => set_number_property(&gitlab, "updatedAt", updated_at),
        None => remove_property(&gitlab, "updatedAt"),
    }
    Ok(root.to_string())
}

fn set_role_redmine_user_text(
    text: &str,
    role: Role,
    user: &RedmineUser,
) -> Result<String, String> {
    let root = parse_root(text)?;
    let identity = role_entry_object(&root, role).object_value_or_set("redmineUser");
    set_number_property(&identity, "userId", user.user_id as i64);
    set_string_property(&identity, "login", &user.login);
    Ok(root.to_string())
}

fn set_role_legacy_text(
    text: &str,
    role: Role,
    legacy: &RoleLegacyConfig,
) -> Result<String, String> {
    let root = parse_root(text)?;
    let legacy_object = role_entry_object(&root, role).object_value_or_set("legacy");
    set_optional_string_property(&legacy_object, "apiBase", legacy.api_base.as_deref());
    set_optional_string_property(&legacy_object, "repository", legacy.repository.as_deref());
    Ok(root.to_string())
}

/// The `roles.<role>` object, created when it does not exist yet.
fn role_entry_object(root: &CstRootNode, role: Role) -> CstObject {
    root.object_value_or_set()
        .object_value_or_set("roles")
        .object_value_or_set(role.as_str())
}

fn set_string_property(object: &CstObject, name: &str, value: &str) {
    match object.get(name) {
        Some(property) => property.set_value(CstInputValue::String(value.to_owned())),
        None => {
            object.append(name, CstInputValue::String(value.to_owned()));
        }
    }
}

fn set_number_property(object: &CstObject, name: &str, value: i64) {
    match object.get(name) {
        Some(property) => property.set_value(CstInputValue::Number(value.to_string())),
        None => {
            object.append(name, CstInputValue::Number(value.to_string()));
        }
    }
}

/// Set a string property, or remove it when the value is absent.
fn set_optional_string_property(object: &CstObject, name: &str, value: Option<&str>) {
    match value {
        Some(value) => set_string_property(object, name, value),
        None => remove_property(object, name),
    }
}

fn remove_property(object: &CstObject, name: &str) {
    if let Some(property) = object.get(name) {
        property.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::config_file::ConfigFile;
    use crate::policy::Role;

    const BASE: &str = r#"{
  // machine-wide settings
  "version": 1,
  "settings": {
    "PHASEGENT_DEFAULT_PROVIDER": "redmine", // keep this comment
  },
  "roles": {
    "executor": {
      "provider": "redmine"
    }
  }
}"#;

    #[test]
    fn strict_profile_accepts_comments_and_trailing_commas() {
        let document = ConfigDocument::parse(BASE).expect("comments and trailing commas are JSONC");
        assert_eq!(
            document.model().get_setting("PHASEGENT_DEFAULT_PROVIDER"),
            Some("redmine")
        );
        assert_eq!(document.model().provider(Role::Executor), Some("redmine"));
    }

    #[test]
    fn strict_profile_rejects_other_json5_extensions() {
        for (label, text) in [
            ("single quotes", r#"{'version': 1}"#),
            ("unquoted key", r#"{version: 1}"#),
            ("missing comma", r#"{"version": 1 "settings": {}}"#),
            ("hex number", r#"{"version": 0x1}"#),
            ("unary plus", r#"{"version": +1}"#),
            ("bare decimal", r#"{"version": .5}"#),
            (
                "extended escape",
                r#"{"version": 1, "settings": {"PHASEGENT_NOTIFY_NTFY_TOPIC": "\x41"}}"#,
            ),
        ] {
            assert!(
                ConfigDocument::parse(text).is_err(),
                "{label} must be rejected by the strict JSONC profile"
            );
        }
    }

    #[test]
    fn duplicate_keys_are_rejected_without_echoing_values() {
        let text = r#"{
  "version": 1,
  "settings": {
    "PHASEGENT_NOTIFY_NTFY_TOKEN": "first-secret",
    "PHASEGENT_NOTIFY_NTFY_TOKEN": "second-secret"
  }
}"#;
        let error = ConfigDocument::parse(text).unwrap_err();
        assert!(error.contains("duplicate object key"));
        assert!(!error.contains("first-secret"));
        assert!(!error.contains("second-secret"));
    }

    #[test]
    fn unknown_fields_roles_and_settings_are_rejected() {
        for text in [
            r#"{"version": 1, "surprise": true}"#,
            r#"{"version": 1, "settings": {"PHASEGENT_UNKNOWN": "x"}}"#,
            r#"{"version": 1, "roles": {"intruder": {}}}"#,
            r#"{"version": 1, "roles": {"executor": {"provider": "redmine", "surprise": true}}}"#,
            r#"{"version": 1, "roles": {"executor": {"credentials": {"bitbucket": {"value": "x"}}}}}"#,
        ] {
            assert!(ConfigDocument::parse(text).is_err(), "must reject: {text}");
        }
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let error = ConfigDocument::parse(r#"{"version": 2}"#).unwrap_err();
        assert!(error.contains("unsupported configuration version 2"));
    }

    #[test]
    fn malformed_text_and_type_errors_never_echo_secrets() {
        let malformed = r#"{
  "version": 1,
  "settings": {
    "PHASEGENT_NOTIFY_NTFY_TOKEN": "super-secret-token"
"#;
        let error = ConfigDocument::parse(malformed).unwrap_err();
        assert!(!error.contains("super-secret-token"));

        let wrong_type = r#"{
  "version": 1,
  "settings": {
    "PHASEGENT_NOTIFY_NTFY_TOKEN": "super-secret-token"
  },
  "roles": {
    "executor": {
      "redmine": { "closeStatusId": "super-secret-token" }
    }
  }
}"#;
        let error = ConfigDocument::parse(wrong_type).unwrap_err();
        assert!(!error.contains("super-secret-token"));
    }

    #[test]
    fn setting_mutations_preserve_comments_and_unrelated_fields() {
        let mut document = ConfigDocument::parse(BASE).unwrap();
        document
            .set_setting("PHASEGENT_DEFAULT_PROVIDER", "forgejo")
            .unwrap();
        let text = document.as_str();
        assert!(text.contains("// machine-wide settings"));
        assert!(text.contains("// keep this comment"));
        assert!(text.contains(r#""PHASEGENT_DEFAULT_PROVIDER": "forgejo""#));
        assert_eq!(
            document.model().get_setting("PHASEGENT_DEFAULT_PROVIDER"),
            Some("forgejo")
        );

        assert!(
            document
                .clear_setting("PHASEGENT_DEFAULT_PROVIDER")
                .unwrap()
        );
        assert!(!document.as_str().contains("PHASEGENT_DEFAULT_PROVIDER"));
        assert!(document.as_str().contains("// machine-wide settings"));
        // A comment attached to the removed property belongs to that key and
        // is removed with it; comments elsewhere stay untouched.
        assert!(!document.as_str().contains("// keep this comment"));
        assert_eq!(
            document
                .model()
                .role(Role::Executor)
                .and_then(|entry| entry.provider.as_deref()),
            Some("redmine")
        );
    }

    #[test]
    fn role_provider_and_credential_mutations_round_trip() {
        let mut document = ConfigDocument::parse(BASE).unwrap();
        document
            .set_credential(Role::Executor, "redmine", "secret-value", Some(1700000000))
            .unwrap();
        assert_eq!(
            document
                .model()
                .credential(Role::Executor, "redmine")
                .map(|entry| (entry.value.as_str(), entry.updated_at)),
            Some(("secret-value", Some(1700000000)))
        );
        assert!(document.as_str().contains("// keep this comment"));

        document
            .set_role_provider(Role::Executor, Some("forgejo"))
            .unwrap();
        assert_eq!(document.model().provider(Role::Executor), Some("forgejo"));

        document.set_role_provider(Role::Executor, None).unwrap();
        assert_eq!(document.model().provider(Role::Executor), None);
        ConfigDocument::parse(document.as_str()).expect("mutated document stays valid");
    }

    #[test]
    fn new_document_is_an_empty_version_one_document() {
        let document = ConfigDocument::new();
        let reparsed = ConfigDocument::parse(document.as_str()).unwrap();
        assert_eq!(reparsed.model(), &ConfigFile::new());
    }

    #[test]
    fn role_scoped_provider_configs_round_trip_through_the_cst() {
        let mut document = ConfigDocument::parse(BASE).unwrap();
        document
            .set_role_redmine_config(
                Role::Executor,
                RedmineRoleConfig {
                    api_base: Some("https://redmine.example".to_owned()),
                    project_id: None,
                    close_status_id: Some(5),
                },
            )
            .unwrap();
        document
            .set_role_forgejo_config(
                Role::Executor,
                ForgejoRoleConfig {
                    api_base: "https://forgejo.example".to_owned(),
                    repository: "owner/repo".to_owned(),
                    source: "admin-auth-setup".to_owned(),
                    updated_at: 1_700_000_000,
                },
            )
            .unwrap();
        document
            .set_role_gitlab_config(
                Role::Reviewer,
                GitlabRoleConfig {
                    api_base: Some("https://gitlab.example".to_owned()),
                    project_id: Some(42),
                    repository: Some("owner/repo".to_owned()),
                    source: Some("admin-auth-setup".to_owned()),
                    updated_at: Some(1_700_000_001),
                },
            )
            .unwrap();
        document
            .set_role_redmine_user(
                Role::Executor,
                RedmineUser {
                    user_id: 7,
                    login: "phasegent-executor".to_owned(),
                },
            )
            .unwrap();

        let model = document.model();
        let redmine = model.redmine(Role::Executor).unwrap();
        assert_eq!(redmine.api_base.as_deref(), Some("https://redmine.example"));
        assert_eq!(redmine.close_status_id, Some(5));
        assert!(redmine.project_id.is_none());
        let forgejo = model.forgejo(Role::Executor).unwrap();
        assert_eq!(forgejo.repository, "owner/repo");
        assert_eq!(forgejo.updated_at, 1_700_000_000);
        let gitlab = model.gitlab(Role::Reviewer).unwrap();
        assert_eq!(gitlab.project_id, Some(42));
        assert_eq!(gitlab.updated_at, Some(1_700_000_001));
        assert_eq!(
            model.redmine_user(Role::Executor).map(|user| user.user_id),
            Some(7)
        );

        // The unrelated comment and the other role entry survive.
        assert!(document.as_str().contains("// machine-wide settings"));
        assert_eq!(
            model.provider(Role::Executor),
            Some("redmine"),
            "the role provider is untouched"
        );
        ConfigDocument::parse(document.as_str()).expect("mutated document stays valid");
    }

    #[test]
    fn an_optional_role_field_is_removed_when_it_is_cleared() {
        let mut document = ConfigDocument::parse(BASE).unwrap();
        document
            .set_role_redmine_config(
                Role::Executor,
                RedmineRoleConfig {
                    api_base: Some("https://redmine.example".to_owned()),
                    project_id: None,
                    close_status_id: Some(5),
                },
            )
            .unwrap();
        document
            .set_role_redmine_config(
                Role::Executor,
                RedmineRoleConfig {
                    api_base: None,
                    project_id: None,
                    close_status_id: Some(5),
                },
            )
            .unwrap();
        assert!(
            !document.as_str().contains("apiBase"),
            "the cleared field must be gone: {}",
            document.as_str()
        );
        assert!(document.model().redmine(Role::Executor).is_some());
    }
}
