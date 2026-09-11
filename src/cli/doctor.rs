//! `doctor`: read-only self-check for operators and agents.
//!
//! Reports credential *presence* (fingerprint + store time, never
//! values), the resolved issue-index backend, and the masked
//! PostgreSQL URL. It is the approved replacement for schema dumps,
//! `substr(credential, ...)` peeks, and raw `global_setting` reads:
//! everything an agent may legitimately want to confirm about local
//! state, with nothing it must not see. No `--role` required.

use crate::config_snapshot::{self, RoleSnapshot};
use crate::policy::Role;
use serde::Serialize;

/// Index backend state as reported by `doctor`. PostgreSQL entries
/// never carry the raw URL: `pg_masked` keeps host/database with
/// userinfo, query, and fragment stripped.
#[derive(Debug, Serialize)]
pub struct IndexStatus {
    pub backend: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sqlite_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sqlite_exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pg_url_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pg_masked: Option<String>,
}

/// Top-level `doctor` envelope. Field order is part of the output
/// contract; reorder only after confirming downstream tooling
/// tolerates it.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub database_path: String,
    pub roles: Vec<RoleSnapshot>,
    pub global_default_provider: Option<&'static str>,
    pub index: IndexStatus,
}

pub(crate) fn execute_doctor() -> i32 {
    match build_report() {
        Ok(report) => super::print_json(&report),
        Err(message) => {
            super::structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
        }
    }
}

pub(crate) fn build_report() -> Result<DoctorReport, String> {
    let storage = super::open_storage()?;
    let mut roles = Vec::new();
    for role in [
        Role::Admin,
        Role::Orchestrator,
        Role::Executor,
        Role::Reviewer,
        Role::Tester,
    ] {
        roles.push(config_snapshot::snapshot_role(&storage, role)?);
    }
    let global_default_provider = match storage.load_global_setting("PHASEGENT_DEFAULT_PROVIDER")? {
        Some(raw) => Some(
            raw.parse::<crate::providers::config::ProviderKind>()
                .map_err(|error| {
                    format!("persisted PHASEGENT_DEFAULT_PROVIDER is invalid: {error}")
                })?
                .as_str(),
        ),
        None => None,
    };
    let backend = crate::infra::issue_index_backend::resolve_index_backend(&storage)?;
    let index = match backend {
        crate::infra::issue_index_backend::IndexBackendKind::Sqlite => {
            // Mirror `SqliteIssueIndex::open` path selection without
            // opening (or creating) anything: explicit env override
            // wins, otherwise the platform config directory.
            let path = std::env::var_os("PHASEGENT_INDEX_DB_PATH")
                .map(std::path::PathBuf::from)
                .unwrap_or(crate::infra::issue_index::project_dirs_index_path()?);
            let exists = std::path::Path::new(&path).exists();
            IndexStatus {
                backend: "sqlite",
                sqlite_path: Some(path.display().to_string()),
                sqlite_exists: Some(exists),
                pg_url_present: None,
                pg_masked: None,
            }
        }
        crate::infra::issue_index_backend::IndexBackendKind::Postgres => {
            let url = crate::infra::issue_index_backend::resolve_pg_url(&storage)?;
            IndexStatus {
                backend: "postgres",
                sqlite_path: None,
                sqlite_exists: None,
                pg_url_present: Some(url.is_some()),
                pg_masked: url.as_deref().map(config_snapshot::sanitize_url),
            }
        }
    };
    Ok(DoctorReport {
        database_path: storage.db_path().display().to_string(),
        roles,
        global_default_provider,
        index,
    })
}
