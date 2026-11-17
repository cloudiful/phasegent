//! The explicit, human-operated upgrade of a version-1 configuration file.
//!
//! The runtime never reads or adapts a version-1 document: it refuses one
//! with the instruction to run this command. The upgrade is therefore an
//! operator action with an explicit contract:
//!
//!   1. The configuration lock is held for the whole operation, so no normal
//!      configuration writer can change the file while it runs.
//!   2. The file is read as a version-1 document under the strict rules: an
//!      unknown field, a duplicate key, an unknown setting, an empty value,
//!      and a value the version-1 validators rejected are all refusals.
//!   3. A file that already uses schema version 2 is an explicit no-op: no
//!      backup is written and nothing is changed.
//!   4. The version-2 document is rendered from the original text, so every
//!      comment the operator wrote survives and moves with its value; the
//!      role subtree is copied verbatim.
//!   5. The rendered document is parsed again and compared with the model the
//!      migration produced. A difference is a refusal, not a write.
//!   6. A unique, owner-only backup of the exact version-1 bytes is created
//!      before the file is replaced. It is never overwritten and never
//!      removed, not even when a later step fails.
//!   7. The file is replaced atomically with owner-only permissions, and only
//!      when its current bytes are still exactly the bytes that were read.
//!
//! The report carries counts, paths, and the name of a removed deprecated
//! setting. No configuration value and no credential ever appears in it.

pub mod transform;
pub mod v1;

use crate::infra::config_file::runtime;
use crate::infra::config_file::setting_path::{
    GLOBAL_SETTINGS, GlobalSetting, global_setting_by_legacy_name,
};
use crate::infra::config_file::{ConfigDocument, ConfigFile, parse_root};
use crate::infra::config_file::{SettingKind, SettingValue};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The outcome of one upgrade run.
///
/// Only paths, counts, and the name of a removed deprecated setting are
/// reported. No configuration value and no credential appears here.
#[derive(Clone, Debug, Serialize)]
pub struct UpgradeReport {
    /// Absolute path of the configuration file.
    pub config_path: String,
    /// True when the file was rewritten; false for the explicit no-op on a
    /// file that already uses schema version 2.
    pub upgraded: bool,
    /// Absolute path of the backup of the exact version-1 bytes. `None` for
    /// the no-op, which writes nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    /// Number of machine-wide settings that were moved into the typed tree.
    pub settings_migrated: usize,
    /// Number of role entries that were carried over.
    pub roles_preserved: usize,
    /// Number of credential entries that were carried over.
    pub credentials_preserved: usize,
    /// The deprecated settings that were dropped on purpose.
    pub removed_settings: Vec<String>,
}

/// The counter that keeps two backups of the same run apart.
static BACKUP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Upgrade the configuration file to schema version 2.
pub fn upgrade() -> Result<UpgradeReport, String> {
    let store = runtime::store()?;
    let path = store.path().to_path_buf();
    if !path.exists() {
        return Err(format!(
            "there is no configuration file at {} to upgrade",
            path.display()
        ));
    }
    // The lock is held for the whole operation: no normal configuration
    // writer can change the file between the read and the replacement.
    let _lock = store.lock()?;
    let bytes = store
        .read_bytes()?
        .ok_or_else(|| "the configuration file disappeared".to_owned())?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "the configuration file is not valid UTF-8".to_owned())?;
    let root = parse_root(text)?;
    let version = detect_version(&root)?;
    match version {
        2 => no_op(&path, text),
        1 => upgrade_v1(&store, &path, &bytes, text, &root),
        other => Err(format!(
            "unsupported configuration version {other}; the upgrader reads version 1 and \
             recognises version 2"
        )),
    }
}

/// The schema version the document declares.
fn detect_version(root: &jsonc_parser::cst::CstRootNode) -> Result<u32, String> {
    let Some(value) = root.value() else {
        return Err("the configuration file must contain a top-level object".to_owned());
    };
    let Some(object) = value.as_object() else {
        return Err("the configuration file must contain a top-level object".to_owned());
    };
    for property in object.properties() {
        if property.decoded_name().as_deref() != Some("version") {
            continue;
        }
        let Some(node) = property.value() else {
            return Err("the configuration file has no schema version".to_owned());
        };
        return node
            .to_string()
            .trim()
            .parse::<u32>()
            .map_err(|_| "the configuration file has no usable schema version".to_owned());
    }
    Err(
        "the configuration file does not declare a schema version; refusing to upgrade it"
            .to_owned(),
    )
}

/// The explicit no-op: the file already uses schema version 2.
fn no_op(path: &Path, text: &str) -> Result<UpgradeReport, String> {
    // The file is read with the runtime rules, so a file that declares
    // version 2 but does not hold a valid version-2 document is reported as
    // the error it is, not as an up-to-date file.
    let document = ConfigDocument::parse(text)?;
    let model = document.model();
    Ok(UpgradeReport {
        config_path: path.display().to_string(),
        upgraded: false,
        backup_path: None,
        settings_migrated: 0,
        roles_preserved: model.roles.len(),
        credentials_preserved: credential_count(model),
        removed_settings: Vec::new(),
    })
}

/// Upgrade a version-1 document.
fn upgrade_v1(
    store: &crate::infra::config_file::ConfigFileStore,
    path: &Path,
    bytes: &[u8],
    text: &str,
    root: &jsonc_parser::cst::CstRootNode,
) -> Result<UpgradeReport, String> {
    let document = v1::parse_document(text)?;
    let (migrated, removed) = map_settings(&document)?;
    let expected = expected_model(&document, &migrated)?;
    let rendered = transform::render(root, &migrated)?;

    // The rendered document is verified against the model the migration
    // produced before anything is written.
    let verified = ConfigDocument::parse(&rendered)?;
    verify_source_text(&document, verified.model())?;
    if verified.model() != &expected {
        return Err(
            "the upgraded configuration could not be verified against the migrated values; \
             refusing to write it"
                .to_owned(),
        );
    }

    // The backup of the exact version-1 bytes is created before the file is
    // replaced. It is exclusive, so an existing backup is never overwritten,
    // and it is never removed again.
    let backup = create_backup(path, bytes)?;

    // The replacement is atomic and private, and it only happens when the
    // file still holds exactly the bytes that were read.
    store.replace_locked(bytes, &rendered)?;

    Ok(UpgradeReport {
        config_path: path.display().to_string(),
        upgraded: true,
        backup_path: Some(backup.display().to_string()),
        settings_migrated: migrated.len(),
        roles_preserved: document.roles.len(),
        credentials_preserved: credential_count(&expected),
        removed_settings: removed,
    })
}

/// Map the version-1 settings onto their typed destinations.
///
/// A setting the version-2 tree does not know is a refusal. The deprecated
/// index-backend selector has no destination: it was always inert, so it is
/// dropped on purpose and reported.
fn map_settings(
    document: &v1::V1Document,
) -> Result<(Vec<transform::MigratedSetting>, Vec<String>), String> {
    let mut migrated: Vec<transform::MigratedSetting> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    for setting in &document.settings {
        if setting.name == v1::LEGACY_INDEX_BACKEND {
            removed.push(setting.name.clone());
            continue;
        }
        let Some(global) = global_setting_by_legacy_name(&setting.name) else {
            // The setting name comes from the file and is never echoed back:
            // a name can be an accidentally pasted secret.
            return Err(
                "the configuration file holds a setting that has no destination in the typed \
                 configuration tree; refusing to upgrade it"
                    .to_owned(),
            );
        };
        let value = typed_value(global, &setting.value)?;
        migrated.push(transform::MigratedSetting {
            setting: global,
            value,
            comments: setting.comments.clone(),
        });
    }
    // The registry order is the document order of the typed tree, so the
    // upgraded file reads like the canonical schema.
    migrated.sort_by_key(|entry| {
        GLOBAL_SETTINGS
            .iter()
            .position(|setting| setting.dotted == entry.setting.dotted)
            .unwrap_or(usize::MAX)
    });
    Ok((migrated, removed))
}

fn verify_source_text(document: &v1::V1Document, model: &ConfigFile) -> Result<(), String> {
    for source in &document.settings {
        let Some(setting) = global_setting_by_legacy_name(&source.name) else {
            continue;
        };
        if setting.kind == SettingKind::Text
            && model.setting(setting.dotted) != Some(SettingValue::Text(source.value.clone()))
        {
            return Err("the upgraded configuration does not preserve a source text value; refusing to write it".to_owned());
        }
    }
    Ok(())
}

/// The version-2 model the migration produces.
fn expected_model(
    document: &v1::V1Document,
    migrated: &[transform::MigratedSetting],
) -> Result<ConfigFile, String> {
    let mut model = ConfigFile::new();
    model.roles = document.roles.clone();
    for entry in migrated {
        model.set_setting(entry.setting.dotted, entry.value.clone())?;
    }
    model.validate()?;
    Ok(model)
}

/// Convert a persisted version-1 value into its typed form.
///
/// A text value keeps its bytes; a boolean, a port, a list, and a literal are
/// converted the way the runtime parsers read them, so the effective
/// behaviour is unchanged. A value the typed tree cannot represent is a
/// refusal, and the value never appears in the error.
fn typed_value(setting: &GlobalSetting, raw: &str) -> Result<SettingValue, String> {
    let trimmed = raw.trim();
    match setting.kind {
        SettingKind::Text => Ok(SettingValue::Text(raw.to_owned())),
        SettingKind::Bool => crate::config_write::parse_bool_literal(trimmed)
            .map(SettingValue::Bool)
            .ok_or_else(|| {
                "the configuration file holds a value the typed configuration cannot represent; \
                 refusing to upgrade it"
                    .to_owned()
            }),
        SettingKind::Port => {
            let port = trimmed.parse::<u16>().map_err(|_| {
                "the configuration file holds a value the typed configuration cannot represent; \
                 refusing to upgrade it"
                    .to_owned()
            })?;
            if port == 0 {
                return Err(
                    "the configuration file holds a value the typed configuration cannot \
                     represent; refusing to upgrade it"
                        .to_owned(),
                );
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
                return Err(
                    "the configuration file holds a value the typed configuration cannot \
                     represent; refusing to upgrade it"
                        .to_owned(),
                );
            }
            Ok(SettingValue::List(values))
        }
        SettingKind::Choice(allowed) => {
            let normalized = match setting.legacy_name {
                "PHASEGENT_NOTIFY_CHANNEL"
                | "PHASEGENT_NOTIFY_EMAIL_TLS"
                | "PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE" => trimmed.to_ascii_lowercase(),
                _ => trimmed.to_owned(),
            };
            allowed
                .iter()
                .find(|literal| **literal == normalized)
                .map(|literal| SettingValue::Text((*literal).to_owned()))
                .ok_or_else(|| {
                    "the configuration file holds a value the typed configuration cannot \
                     represent; refusing to upgrade it"
                        .to_owned()
                })
        }
    }
}

/// The number of credential entries a model carries.
fn credential_count(model: &ConfigFile) -> usize {
    model
        .roles
        .values()
        .map(|role| role.credentials.len())
        .sum()
}

/// Create the private backup of the exact version-1 bytes.
///
/// The file is created exclusively, so an existing backup is never
/// overwritten, and it is created owner-only.
fn create_backup(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "the configuration path has no parent directory".to_owned())?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "phasegent.jsonc".to_owned());
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = BACKUP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let backup = parent.join(format!(
        "{file_name}.v1-backup-{stamp}-{}-{counter}.jsonc",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&backup).map_err(|error| {
        format!(
            "could not create the configuration backup at {}: {error}",
            backup.display()
        )
    })?;
    file.write_all(bytes).map_err(|error| {
        format!(
            "could not write the configuration backup at {}: {error}",
            backup.display()
        )
    })?;
    file.sync_all().map_err(|error| {
        format!(
            "could not sync the configuration backup at {}: {error}",
            backup.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o600)).map_err(|error| {
            format!(
                "could not secure the configuration backup at {}: {error}",
                backup.display()
            )
        })?;
    }
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = r#"{
  // the machine-wide settings
  "version": 1,
  "settings": {
    // the default provider
    "PHASEGENT_DEFAULT_PROVIDER": "redmine",
    "PHASEGENT_NOTIFY_ENABLED": "true", // notifications are on
    "PHASEGENT_NOTIFY_EMAIL_SMTP_PORT": "587",
    "PHASEGENT_NOTIFY_EMAIL_TLS": "starttls",
    "PHASEGENT_NOTIFY_EMAIL_TO": "ops@example.com, oncall@example.com",
    "PHASEGENT_INDEX_BACKEND": "sqlite",
    "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY": "mirror-secret"
  },
  // the role entries
  "roles": {
    "executor": {
      "provider": "redmine",
      "credentials": {
        "redmine": { "value": "role-secret", "updatedAt": 1700000000 }
      }
    }
  }
}
"#;

    fn document(text: &str) -> v1::V1Document {
        v1::parse_document(text).expect("the version-1 document is readable")
    }

    fn render(text: &str) -> String {
        let document = document(text);
        let root = parse_root(text).expect("the document parses");
        let (migrated, _) = map_settings(&document).expect("the settings map");
        transform::render(&root, &migrated).expect("the document renders")
    }

    #[test]
    fn source_text_and_legacy_choices_survive_upgrade() {
        let text = r#"{"version":1,"settings":{"PHASEGENT_INDEX_PG_URL":"  postgres://example/db  ","PHASEGENT_NOTIFY_CHANNEL":"EMAIL","PHASEGENT_NOTIFY_EMAIL_TLS":"STARTTLS","PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE":"MARKDOWN"}}"#;
        let source = document(text);
        let rendered = render(text);
        let mut model = ConfigDocument::parse(&rendered).unwrap().model().clone();
        verify_source_text(&source, &model).unwrap();
        assert_eq!(
            model.setting("index.postgres.url"),
            Some(SettingValue::Text("  postgres://example/db  ".into()))
        );
        assert_eq!(
            model.setting("notifications.channel"),
            Some(SettingValue::Text("email".into()))
        );
        assert_eq!(
            model.setting("notifications.email.smtp.tls"),
            Some(SettingValue::Text("starttls".into()))
        );
        assert_eq!(
            model.setting("notifications.dingtalk.messageType"),
            Some(SettingValue::Text("markdown".into()))
        );
        model
            .set_setting(
                "index.postgres.url",
                SettingValue::Text("postgres://example/db".into()),
            )
            .unwrap();
        assert!(verify_source_text(&source, &model).is_err());
    }

    #[test]
    fn legacy_provider_is_not_case_folded() {
        let source = r#"{"version":1,"settings":{"PHASEGENT_DEFAULT_PROVIDER":"REDMINE"}}"#;
        assert!(v1::parse_document(source).is_err());
        let setting = global_setting_by_legacy_name("PHASEGENT_DEFAULT_PROVIDER").unwrap();
        assert!(typed_value(setting, "REDMINE").is_err());
        assert_eq!(
            typed_value(setting, " redmine ").unwrap(),
            SettingValue::Text("redmine".into())
        );
    }

    #[test]
    fn all_comments_survive_empty_and_removed_settings() {
        for settings in ["", "// removed\n\"PHASEGENT_INDEX_BACKEND\":\"sqlite\","] {
            let text = format!(
                "// outer header\n{{ // root start\n\"version\":1, /* before settings */ \"settings\":{{ {settings} // settings tail\n }}, // before roles\n\"roles\":{{ /* role comment */ }}, // root tail\n}}\n// outer footer\n"
            );
            let output = render(&text);
            ConfigDocument::parse(&output).unwrap();
            for comment in [
                "// outer header",
                "// root start",
                "/* before settings */",
                "// settings tail",
                "// before roles",
                "/* role comment */",
                "// root tail",
                "// outer footer",
            ] {
                assert!(output.contains(comment), "missing {comment}: {output}");
            }
            if !settings.is_empty() {
                assert!(output.contains("// removed"));
            }
            assert!(output.starts_with("// outer header"));
        }
    }

    #[test]
    fn a_version_one_document_becomes_a_typed_version_two_document() {
        let rendered = render(V1);
        let document = ConfigDocument::parse(&rendered).expect("the rendered document is valid");
        let model = document.model();
        assert_eq!(
            model.setting("provider"),
            Some(SettingValue::Text("redmine".to_owned()))
        );
        assert_eq!(
            model.setting("notifications.enabled"),
            Some(SettingValue::Bool(true))
        );
        assert_eq!(
            model.setting("notifications.email.smtp.port"),
            Some(SettingValue::Port(587))
        );
        assert_eq!(
            model.setting("notifications.email.smtp.tls"),
            Some(SettingValue::Text("starttls".to_owned()))
        );
        assert_eq!(
            model.setting("notifications.email.to"),
            Some(SettingValue::List(vec![
                "ops@example.com".to_owned(),
                "oncall@example.com".to_owned()
            ]))
        );
        assert_eq!(
            model.setting("redmine.gitMirror.apiKey"),
            Some(SettingValue::Text("mirror-secret".to_owned()))
        );
        assert_eq!(
            model.setting("index.postgres.url"),
            None,
            "the inert index backend selector has no destination"
        );
    }

    #[test]
    fn the_role_entries_and_their_credentials_are_carried_over() {
        let rendered = render(V1);
        let document = ConfigDocument::parse(&rendered).expect("the rendered document is valid");
        let executor = document
            .model()
            .role(crate::policy::Role::Executor)
            .unwrap();
        assert_eq!(executor.provider.as_deref(), Some("redmine"));
        let credential = executor.credentials.get("redmine").unwrap();
        assert_eq!(credential.value, "role-secret");
        assert_eq!(credential.updated_at, Some(1_700_000_000));
    }

    #[test]
    fn the_comments_survive_and_move_with_their_value() {
        let rendered = render(V1);
        assert!(
            rendered.contains("// the default provider"),
            "the comment of a setting must survive: {rendered}"
        );
        assert!(
            rendered.contains("// notifications are on"),
            "a trailing comment must survive: {rendered}"
        );
        assert!(
            rendered.contains("// the machine-wide settings"),
            "the header comment must survive: {rendered}"
        );
        assert!(
            rendered.contains("// the role entries"),
            "the comment of the roles section must survive: {rendered}"
        );
        // The comment of a setting stays with the value it belonged to.
        let provider_line = rendered
            .lines()
            .position(|line| line.contains("\"provider\""))
            .expect("the provider is rendered");
        assert!(
            rendered
                .lines()
                .take(provider_line)
                .any(|line| line.contains("// the default provider")),
            "the comment must move with the value: {rendered}"
        );
    }

    #[test]
    fn the_deprecated_index_backend_selector_is_dropped_on_purpose() {
        let document = document(V1);
        let (migrated, removed) = map_settings(&document).expect("the settings map");
        assert_eq!(removed, vec![v1::LEGACY_INDEX_BACKEND.to_owned()]);
        assert!(
            migrated
                .iter()
                .all(|entry| entry.setting.dotted != "index.postgres.url")
        );
    }

    #[test]
    fn an_unknown_setting_is_refused_without_echoing_it() {
        let text = r#"{"version": 1, "settings": {"PHASEGENT_SECRET_UNKNOWN": "x"}}"#;
        let error = v1::parse_document(text).expect_err("an unknown setting must be refused");
        assert!(error.contains("does not know"), "got: {error}");
        assert!(!error.contains("SECRET_UNKNOWN"), "got: {error}");
    }

    #[test]
    fn an_empty_value_is_refused() {
        let text = r#"{"version": 1, "settings": {"PHASEGENT_REDMINE_API_BASE": "  "}}"#;
        let error = v1::parse_document(text).expect_err("an empty value must be refused");
        assert!(error.contains("empty value"), "got: {error}");
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let text = r#"{"version": 1, "surprise": true}"#;
        let error = v1::parse_document(text).expect_err("an unknown field must be refused");
        assert!(error.contains("does not define"), "got: {error}");
        assert!(!error.contains("surprise"), "got: {error}");
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let text = r#"{"version": 3, "settings": {}}"#;
        let error = v1::parse_document(text).expect_err("an unknown version must be refused");
        assert!(
            error.contains("unsupported configuration version 3"),
            "got: {error}"
        );
    }

    #[test]
    fn an_unknown_role_is_refused() {
        let text = r#"{"version": 1, "settings": {}, "roles": {"tester": {}}}"#;
        let document = v1::parse_document(text).expect("the document decodes");
        let (migrated, _) = map_settings(&document).expect("the settings map");
        let error =
            expected_model(&document, &migrated).expect_err("an unknown role must be refused");
        assert!(error.contains("unknown role"), "got: {error}");
    }

    #[test]
    fn a_duplicate_key_is_refused() {
        let text = r#"{"version": 1, "settings": {"PHASEGENT_DEFAULT_PROVIDER": "redmine", "PHASEGENT_DEFAULT_PROVIDER": "local"}}"#;
        let error = v1::parse_document(text).expect_err("a duplicate key must be refused");
        assert!(error.contains("duplicate"), "got: {error}");
    }

    #[test]
    fn a_version_two_document_is_recognised_as_up_to_date() {
        let text = r#"{"version": 2, "roles": {}}"#;
        let root = parse_root(text).expect("the document parses");
        assert_eq!(detect_version(&root).expect("the version is read"), 2);
        let report = no_op(Path::new("/tmp/phasegent.jsonc"), text).expect("the no-op succeeds");
        assert!(!report.upgraded);
        assert!(report.backup_path.is_none());
    }
}
