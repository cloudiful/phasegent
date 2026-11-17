use super::model::{
    ConfigFile, ForgejoRoleConfig, GitlabRoleConfig, RedmineRoleConfig, RedmineUser,
    RoleLegacyConfig, SettingValue,
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
                "{{\n  \"version\": {},\n  \"roles\": {{}}\n}}\n",
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

    /// Store one machine-wide setting. The value is written with its JSON
    /// type, so a boolean stays a boolean, a port stays a number, and a
    /// list stays an array.
    pub fn set_setting(&mut self, path: &str, value: &SettingValue) -> Result<(), String> {
        let mut model = self.model.clone();
        model.set_setting(path, value.clone())?;
        let text = set_setting_text(
            &self.text,
            crate::infra::config_file::setting_path::setting_segments(path)
                .ok_or_else(|| "unknown configuration setting".to_owned())?,
            value,
        )?;
        self.replace(text, model)
    }

    /// Remove one machine-wide setting. Only the addressed leaf is
    /// removed: sibling values and the comments around them survive.
    pub fn clear_setting(&mut self, path: &str) -> Result<bool, String> {
        let mut model = self.model.clone();
        let removed = model.clear_setting(path);
        if !removed {
            return Ok(false);
        }
        let text = clear_setting_text(
            &self.text,
            crate::infra::config_file::setting_path::setting_segments(path)
                .ok_or_else(|| "unknown configuration setting".to_owned())?,
        )?;
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
    // The version decides how the document is read. A version-1 file is
    // refused with an actionable upgrade instruction before any other
    // field is interpreted, so the operator never sees a confusing
    // "unknown field" error for the flat settings map.
    check_version(&value)?;
    let model: ConfigFile =
        serde_json::from_value(value).map_err(|error| safe_deserialize_error(&error))?;
    model.validate()?;
    Ok(model)
}

/// The marker that introduces the refusal of a configuration file this
/// build cannot read. A file that uses another schema version needs an
/// operator upgrade; the marker lets the resolution paths recognise that
/// one refusal without matching on its wording.
pub const UPGRADE_REQUIRED_MARKER: &str = "configuration upgrade required: ";

/// Refuse a document whose version this build cannot read.
fn check_version(value: &serde_json::Value) -> Result<(), String> {
    let Some(version) = value.get("version").and_then(serde_json::Value::as_u64) else {
        return Ok(());
    };
    if version == u64::from(crate::infra::config_file::SCHEMA_VERSION) {
        return Ok(());
    }
    if version == 1 {
        return Err(format!(
            "{UPGRADE_REQUIRED_MARKER}the configuration file uses schema version 1; \
             a human operator must upgrade it with `phasegent admin config upgrade` \
             before phasegent reads or writes it"
        ));
    }
    Err(format!(
        "unsupported configuration version {version}; expected {}",
        crate::infra::config_file::SCHEMA_VERSION
    ))
}

/// Parse a configuration document into its CST. The strict profile is the
/// same one the runtime uses, so the explicit upgrader reads a version-1
/// file with exactly the rules a version-2 file is read with.
pub(crate) fn parse_root(text: &str) -> Result<CstRootNode, String> {
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

/// Refuse a document that repeats an object key. The check runs over the
/// whole tree, so a duplicate key inside a nested section is refused too.
pub(crate) fn check_duplicate_keys(node: &jsonc_parser::cst::CstNode) -> Result<(), String> {
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

/// Write a typed value at `segments`, creating the missing intermediate
/// objects.
fn set_setting_text(text: &str, segments: &[&str], value: &SettingValue) -> Result<String, String> {
    let root = parse_root(text)?;
    let mut object = root.object_value_or_set();
    let Some((last, parents)) = segments.split_last() else {
        return Err("the configuration path is empty".to_owned());
    };
    for segment in parents {
        object = object.object_value_or_set(segment);
    }
    set_property(&object, last, value);
    Ok(root.to_string())
}

/// Remove the value at `segments`. Only the addressed leaf is removed, so
/// sibling values and their comments survive.
fn clear_setting_text(text: &str, segments: &[&str]) -> Result<String, String> {
    let root = parse_root(text)?;
    let mut object = root.object_value_or_set();
    let Some((last, parents)) = segments.split_last() else {
        return Err("the configuration path is empty".to_owned());
    };
    for segment in parents {
        let Some(child) = object.object_value(segment) else {
            return Ok(root.to_string());
        };
        object = child;
    }
    remove_property(&object, last);
    Ok(root.to_string())
}

/// Write one property with the JSON type of the value.
fn set_property(object: &CstObject, name: &str, value: &SettingValue) {
    let input = match value {
        SettingValue::Text(text) => CstInputValue::String(text.clone()),
        SettingValue::Bool(flag) => CstInputValue::Bool(*flag),
        SettingValue::Port(port) => CstInputValue::Number(port.to_string()),
        SettingValue::List(values) => CstInputValue::Array(
            values
                .iter()
                .map(|value| CstInputValue::String(value.clone()))
                .collect(),
        ),
    };
    match object.get(name) {
        Some(property) => property.set_value(input),
        None => {
            object.append(name, input);
        }
    }
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
  "version": 2,
  "provider": "redmine", // keep this comment
  "redmine": {
    "apiBase": "https://redmine-global.example"
  },
  "roles": {
    "executor": {
      "provider": "redmine"
    }
  }
}"#;

    fn text_value(value: &str) -> SettingValue {
        SettingValue::Text(value.to_owned())
    }

    #[test]
    fn strict_profile_accepts_comments_and_trailing_commas() {
        let document = ConfigDocument::parse(BASE).expect("comments and trailing commas are JSONC");
        assert_eq!(
            document.model().setting("provider"),
            Some(text_value("redmine"))
        );
        assert_eq!(document.model().provider(Role::Executor), Some("redmine"));
    }

    #[test]
    fn strict_profile_rejects_other_json5_extensions() {
        for (label, text) in [
            ("single quotes", r#"{'version': 2}"#),
            ("unquoted key", r#"{version: 2}"#),
            ("missing comma", r#"{"version": 2 "roles": {}}"#),
            ("hex number", r#"{"version": 0x2}"#),
            ("unary plus", r#"{"version": +2}"#),
            ("bare decimal", r#"{"version": .5}"#),
            (
                "extended escape",
                r#"{"version": 2, "notifications": {"ntfy": {"topic": "\x41"}}}"#,
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
  "version": 2,
  "notifications": {
    "ntfy": { "token": "first-secret", "token": "second-secret" }
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
            r#"{"version": 2, "surprise": true}"#,
            r#"{"version": 2, "settings": {"PHASEGENT_UNKNOWN": "x"}}"#,
            r#"{"version": 2, "roles": {"intruder": {}}}"#,
            r#"{"version": 2, "roles": {"executor": {"provider": "redmine", "surprise": true}}}"#,
            r#"{"version": 2, "roles": {"executor": {"credentials": {"bitbucket": {"value": "x"}}}}}"#,
            r#"{"version": 2, "notifications": {"surprise": true}}"#,
            r#"{"version": 2, "redmine": {"surprise": true}}"#,
        ] {
            assert!(ConfigDocument::parse(text).is_err(), "must reject: {text}");
        }
    }

    #[test]
    fn a_version_one_file_is_refused_with_an_upgrade_instruction() {
        let error = ConfigDocument::parse(
            r#"{"version": 1, "settings": {"PHASEGENT_DEFAULT_PROVIDER": "redmine"}}"#,
        )
        .unwrap_err();
        assert!(error.contains("schema version 1"), "got: {error}");
        assert!(error.contains("admin config upgrade"), "got: {error}");
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let error = ConfigDocument::parse(r#"{"version": 3}"#).unwrap_err();
        assert!(error.contains("unsupported configuration version 3"));
    }

    #[test]
    fn malformed_text_and_type_errors_never_echo_secrets() {
        let malformed = r#"{
  "version": 2,
  "notifications": {
    "ntfy": { "token": "super-secret-token" }
"#;
        let error = ConfigDocument::parse(malformed).unwrap_err();
        assert!(!error.contains("super-secret-token"));

        let wrong_type = r#"{
  "version": 2,
  "notifications": {
    "ntfy": { "token": "super-secret-token" },
    "enabled": "super-secret-token"
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
            .set_setting("provider", &text_value("forgejo"))
            .unwrap();
        let text = document.as_str();
        assert!(text.contains("// machine-wide settings"));
        assert!(text.contains("// keep this comment"));
        assert!(text.contains(r#""provider": "forgejo""#));
        assert_eq!(
            document.model().setting("provider"),
            Some(text_value("forgejo"))
        );

        assert!(document.clear_setting("provider").unwrap());
        assert!(!document.as_str().contains(r#""provider": "forgejo""#));
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
    fn typed_values_are_written_with_their_json_type() {
        let mut document = ConfigDocument::parse(BASE).unwrap();
        document
            .set_setting("notifications.enabled", &SettingValue::Bool(true))
            .unwrap();
        document
            .set_setting("notifications.email.smtp.port", &SettingValue::Port(587))
            .unwrap();
        document
            .set_setting(
                "notifications.email.to",
                &SettingValue::List(vec!["ops@example.com".to_owned()]),
            )
            .unwrap();
        let text = document.as_str();
        assert!(text.contains(r#""enabled": true"#), "got: {text}");
        assert!(text.contains(r#""port": 587"#), "got: {text}");
        assert!(text.contains(r#""to": ["ops@example.com"]"#), "got: {text}");
        assert_eq!(
            document.model().setting("notifications.enabled"),
            Some(SettingValue::Bool(true))
        );
        assert_eq!(
            document.model().setting("notifications.email.smtp.port"),
            Some(SettingValue::Port(587))
        );
        assert_eq!(
            document.model().setting("notifications.email.to"),
            Some(SettingValue::List(vec!["ops@example.com".to_owned()]))
        );
        // The nested hierarchy was created on the way.
        assert!(text.contains(r#""smtp": {"#), "got: {text}");
    }

    #[test]
    fn clearing_a_leaf_keeps_its_siblings_and_their_comments() {
        let mut document = ConfigDocument::parse(BASE).unwrap();
        document
            .set_setting("notifications.ntfy.token", &text_value("super-secret"))
            .unwrap();
        document
            .set_setting("notifications.ntfy.topic", &text_value("phasegent"))
            .unwrap();
        assert!(document.clear_setting("notifications.ntfy.token").unwrap());
        let text = document.as_str();
        assert!(!text.contains("super-secret"), "got: {text}");
        assert!(text.contains(r#""topic": "phasegent""#), "got: {text}");
        assert_eq!(document.model().setting("notifications.ntfy.token"), None);
        assert_eq!(
            document.model().setting("notifications.ntfy.topic"),
            Some(text_value("phasegent"))
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
    fn new_document_is_an_empty_version_two_document() {
        let document = ConfigDocument::new();
        let reparsed = ConfigDocument::parse(document.as_str()).unwrap();
        assert_eq!(reparsed.model(), &ConfigFile::new());
        assert!(document.as_str().contains(r#""version": 2"#));
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
            !document.as_str().contains("https://redmine.example"),
            "the cleared field must be gone: {}",
            document.as_str()
        );
        assert!(
            document.as_str().contains("https://redmine-global.example"),
            "the machine-wide address must survive: {}",
            document.as_str()
        );
        assert!(document.model().redmine(Role::Executor).is_some());
    }
}
