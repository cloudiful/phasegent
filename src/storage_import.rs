//! One-shot legacy importer for the phasegent SQLite database.
//!
//! `import_legacy` walks the four well-known legacy filenames for each
//! role, copies the populated fields into SQLite, and writes one
//! `import_log` row per legacy file so subsequent opens short-circuit
//! cleanly. The whole import runs inside a single transaction so a
//! corrupt JSON file cannot leave the database half-populated; field
//! updates target only NULL columns so existing SQLite values are
//! preserved by design.

use crate::policy::Role;
use crate::storage::{ImportReport, PROVIDER_FORGEJO, PROVIDER_REDMINE, Storage};
use rusqlite::params;
use std::fs;
use std::path::Path;

/// Import legacy JSON / key / token files from `config_dir` into
/// SQLite. Imports are field-by-field and idempotent: a field is
/// only copied when SQLite already has a NULL or no row for it,
/// and the `import_log` records every successful copy so repeated
/// opens do not re-import an already-populated field.
///
/// Import runs in a single transaction so a corrupt file cannot
/// leave the database half-populated.
pub(crate) fn import_legacy(storage: &Storage, config_dir: &Path) -> Result<ImportReport, String> {
    let mut report = ImportReport::default();
    let transaction = storage
        .connection
        .unchecked_transaction()
        .map_err(|error| format!("could not begin legacy import: {error}"))?;

    for role in [
        Role::Admin,
        Role::Orchestrator,
        Role::Executor,
        Role::Reviewer,
    ] {
        let role_label = role.as_str();
        let role_config_path = config_dir.join(format!("{role_label}.config.json"));
        if let Some(value) = read_json_if_present(&role_config_path)? {
            let provider = legacy_provider(&value);
            let api_base = legacy_str(&value, "api_base");
            let repository = legacy_str(&value, "repository");
            record_change_opt(
                &mut report,
                apply_provider_field(&transaction, role, provider)?,
            );
            record_change_opt(
                &mut report,
                apply_role_api_base_field(&transaction, role, api_base)?,
            );
            record_change_opt(
                &mut report,
                apply_role_repository_field(&transaction, role, repository)?,
            );
            log_import(&transaction, &role_config_path, "role_config")?;
        }

        let redmine_config_path = config_dir.join(format!("redmine.{role_label}.config.json"));
        if let Some(value) = read_json_if_present(&redmine_config_path)? {
            let api_base = legacy_str(&value, "api_base");
            let project_id = legacy_str(&value, "project_id");
            let close_status_id = legacy_u64(&value, "close_status_id");
            record_change_opt(
                &mut report,
                apply_redmine_api_base_field(&transaction, role, api_base)?,
            );
            record_change_opt(
                &mut report,
                apply_redmine_project_id_field(&transaction, role, project_id)?,
            );
            record_change_opt(
                &mut report,
                apply_redmine_close_status_field(&transaction, role, close_status_id)?,
            );
            log_import(&transaction, &redmine_config_path, "redmine_config")?;
        }

        let redmine_key_path = config_dir.join(format!("redmine.{role_label}.key"));
        if let Some(value) = read_trimmed_if_present(&redmine_key_path)? {
            if storage.load_credential(role, PROVIDER_REDMINE)?.is_some() {
                report.skipped += 1;
            } else {
                write_credential_row(&transaction, role, PROVIDER_REDMINE, &value)?;
                log_import(&transaction, &redmine_key_path, "credential")?;
                report.imported += 1;
            }
        }

        let token_path = config_dir.join(format!("{role_label}.token"));
        if let Some(value) = read_trimmed_if_present(&token_path)? {
            if storage.load_credential(role, PROVIDER_FORGEJO)?.is_some() {
                report.skipped += 1;
            } else {
                write_credential_row(&transaction, role, PROVIDER_FORGEJO, &value)?;
                log_import(&transaction, &token_path, "credential")?;
                report.imported += 1;
            }
        }
    }

    transaction
        .commit()
        .map_err(|error| format!("could not commit legacy import: {error}"))?;
    Ok(report)
}

fn record_change(report: &mut ImportReport, changed: bool) {
    if changed {
        report.imported += 1;
    } else {
        report.skipped += 1;
    }
}

fn record_change_opt(report: &mut ImportReport, changed: Option<bool>) {
    if let Some(changed) = changed {
        record_change(report, changed);
    }
}

fn apply_provider_field(
    transaction: &rusqlite::Transaction<'_>,
    role: Role,
    candidate: Option<&str>,
) -> Result<Option<bool>, String> {
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    upsert_role_row(transaction, role)?;
    let affected = transaction
        .execute(
            "UPDATE role_config SET provider = ?1 \
             WHERE role = ?2 AND provider IS NULL",
            params![candidate, role.as_str()],
        )
        .map_err(|error| format!("could not import provider field: {error}"))?;
    Ok(Some(affected > 0))
}

fn apply_role_api_base_field(
    transaction: &rusqlite::Transaction<'_>,
    role: Role,
    candidate: Option<&str>,
) -> Result<Option<bool>, String> {
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    upsert_role_row(transaction, role)?;
    let affected = transaction
        .execute(
            "UPDATE role_config SET api_base = ?1 \
             WHERE role = ?2 AND api_base IS NULL",
            params![candidate, role.as_str()],
        )
        .map_err(|error| format!("could not import role api_base: {error}"))?;
    Ok(Some(affected > 0))
}

fn apply_role_repository_field(
    transaction: &rusqlite::Transaction<'_>,
    role: Role,
    candidate: Option<&str>,
) -> Result<Option<bool>, String> {
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    upsert_role_row(transaction, role)?;
    let affected = transaction
        .execute(
            "UPDATE role_config SET repository = ?1 \
             WHERE role = ?2 AND repository IS NULL",
            params![candidate, role.as_str()],
        )
        .map_err(|error| format!("could not import role repository: {error}"))?;
    Ok(Some(affected > 0))
}

fn apply_redmine_api_base_field(
    transaction: &rusqlite::Transaction<'_>,
    role: Role,
    candidate: Option<&str>,
) -> Result<Option<bool>, String> {
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    upsert_redmine_row(transaction, role)?;
    let affected = transaction
        .execute(
            "UPDATE role_redmine_config SET api_base = ?1 \
             WHERE role = ?2 AND api_base IS NULL",
            params![candidate, role.as_str()],
        )
        .map_err(|error| format!("could not import redmine api_base: {error}"))?;
    Ok(Some(affected > 0))
}

fn apply_redmine_project_id_field(
    transaction: &rusqlite::Transaction<'_>,
    role: Role,
    candidate: Option<&str>,
) -> Result<Option<bool>, String> {
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    upsert_redmine_row(transaction, role)?;
    let affected = transaction
        .execute(
            "UPDATE role_redmine_config SET project_id = ?1 \
             WHERE role = ?2 AND project_id IS NULL",
            params![candidate, role.as_str()],
        )
        .map_err(|error| format!("could not import redmine project_id: {error}"))?;
    Ok(Some(affected > 0))
}

fn apply_redmine_close_status_field(
    transaction: &rusqlite::Transaction<'_>,
    role: Role,
    candidate: Option<u64>,
) -> Result<Option<bool>, String> {
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    upsert_redmine_row(transaction, role)?;
    let affected = transaction
        .execute(
            "UPDATE role_redmine_config SET close_status_id = ?1 \
             WHERE role = ?2 AND close_status_id IS NULL",
            params![candidate, role.as_str()],
        )
        .map_err(|error| format!("could not import redmine close_status_id: {error}"))?;
    Ok(Some(affected > 0))
}

fn upsert_role_row(transaction: &rusqlite::Transaction<'_>, role: Role) -> Result<(), String> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO role_config (role) VALUES (?1)",
            params![role.as_str()],
        )
        .map_err(|error| format!("could not create role_config row: {error}"))?;
    Ok(())
}

fn upsert_redmine_row(transaction: &rusqlite::Transaction<'_>, role: Role) -> Result<(), String> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO role_redmine_config (role) VALUES (?1)",
            params![role.as_str()],
        )
        .map_err(|error| format!("could not create role_redmine_config row: {error}"))?;
    Ok(())
}

fn write_credential_row(
    transaction: &rusqlite::Transaction<'_>,
    role: Role,
    provider: &str,
    credential: &str,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO role_credential (role, provider, credential) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(role, provider) DO UPDATE SET credential = excluded.credential",
            params![role.as_str(), provider, credential],
        )
        .map_err(|error| format!("could not import credential: {error}"))?;
    Ok(())
}

fn log_import(
    transaction: &rusqlite::Transaction<'_>,
    path: &Path,
    field: &str,
) -> Result<(), String> {
    let source = path.to_string_lossy().into_owned();
    transaction
        .execute(
            "INSERT OR IGNORE INTO import_log (source, field) VALUES (?1, ?2)",
            params![source, field],
        )
        .map_err(|error| format!("could not record legacy import: {error}"))?;
    Ok(())
}

fn read_json_if_present(path: &Path) -> Result<Option<serde_json::Value>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("could not parse {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn read_trimmed_if_present(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(value) => {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn legacy_provider(legacy: &serde_json::Value) -> Option<&str> {
    legacy_str(legacy, "provider")
}

fn legacy_str<'a>(legacy: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    legacy.get(field).and_then(|value| {
        if value.is_null() {
            None
        } else {
            value.as_str()
        }
    })
}

fn legacy_u64(legacy: &serde_json::Value, field: &str) -> Option<u64> {
    legacy.get(field).and_then(|value| {
        if value.is_null() {
            None
        } else {
            value.as_u64()
        }
    })
}
