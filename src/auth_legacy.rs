//! Legacy file-based persistence helpers kept for backward compatibility
//! with the original JSON / key / token layout.
//!
//! These helpers exist so existing tests that read and write raw
//! `<role>.config.json` / `redmine.<role>.config.json` /
//! `redmine.<role>.key` files continue to pass after the SQLite
//! migration. The active production path lives in [`crate::auth`] and
//! reads / writes through [`crate::storage`]; the functions here are
//! intentionally limited to the explicit-directory test fixtures.

use crate::policy::Role;
use crate::storage::PROVIDER_REDMINE;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Write a role/provider credential to the legacy file layout.
///
/// Used by the `admin_auth_setup_writes_the_normal_role_scoped_private_key`
/// contract test and by `auth::persist_redmine_bootstrap_for`. The
/// `directory` argument is an explicit path so the helper stays
/// independent of `HOME`.
pub(crate) fn write_credential(
    directory: &Path,
    role: Role,
    provider: &str,
    credential: &str,
) -> Result<(), String> {
    create_private_dir(directory)?;
    let path = match provider {
        "forgejo" => directory.join(format!("{}.token", role.as_str())),
        "redmine" => redmine_key_path_for(directory, role),
        _ => unreachable!("provider was validated above"),
    };
    write_private_file(&path, credential.as_bytes())
}

/// Read the legacy `<role>.config.json` file from `directory`. Returns
/// `Ok(None)` when the file does not exist so callers can treat
/// "absent" and "present but empty" as distinct states.
pub(crate) fn load_config_for(
    directory: &Path,
    role: Role,
) -> Result<Option<crate::auth::StoredConfig>, String> {
    let path = config_path_for(directory, role);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(&path).map_err(|error| format!("could not read configuration: {error}"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("could not parse configuration: {error}"))
}

/// Read the legacy `redmine.<role>.config.json` file from `directory`.
pub(crate) fn load_redmine_config_for(
    directory: &Path,
    role: Role,
) -> Result<Option<crate::auth::RedmineStoredConfig>, String> {
    let path = redmine_config_path_for(directory, role);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)
        .map_err(|error| format!("could not read Redmine configuration: {error}"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("could not parse Redmine configuration: {error}"))
}

/// Persist a bootstrap result to the legacy file layout. Used only by
/// tests; the production path goes through the SQLite `Storage`.
pub(crate) fn persist_redmine_bootstrap_for(
    directory: &Path,
    role: Role,
    api_base: Option<String>,
    project_id: u64,
    close_status_id: u64,
) -> Result<(), String> {
    if project_id == 0 || close_status_id == 0 {
        return Err("Redmine project and close status IDs must be greater than zero".to_owned());
    }
    create_private_dir(directory)?;
    let path = redmine_config_path_for(directory, role);
    let mut config = load_redmine_config_for(directory, role)?.unwrap_or_default();
    if let Some(api_base) = api_base {
        config.api_base = Some(api_base);
    }
    config.project_id = Some(project_id.to_string());
    config.close_status_id = Some(close_status_id);
    write_json_file(&path, &config, "Redmine configuration")?;
    update_provider_for(directory, role, PROVIDER_REDMINE)
}

/// Compute the legacy `<role>.config.json` path under `directory`.
pub(crate) fn config_path_for(directory: &Path, role: Role) -> std::path::PathBuf {
    directory.join(format!("{}.config.json", role.as_str()))
}

/// Compute the legacy `redmine.<role>.config.json` path under `directory`.
pub(crate) fn redmine_config_path_for(directory: &Path, role: Role) -> std::path::PathBuf {
    directory.join(format!("redmine.{}.config.json", role.as_str()))
}

/// Compute the legacy `redmine.<role>.key` path under `directory`.
pub(crate) fn redmine_key_path_for(directory: &Path, role: Role) -> std::path::PathBuf {
    directory.join(format!("redmine.{}.key", role.as_str()))
}

/// Update the `provider` field on the legacy `<role>.config.json`.
/// Companion to `persist_redmine_bootstrap_for` so the role config
/// keeps pointing at the right provider after a Redmine bootstrap.
fn update_provider_for(directory: &Path, role: Role, provider: &str) -> Result<(), String> {
    let path = config_path_for(directory, role);
    let mut config = load_config_for(directory, role)?.unwrap_or_default();
    config.provider = Some(provider.to_owned());
    write_json_file(&path, &config, "configuration")
}

fn write_json_file<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<(), String> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| format!("could not encode {label}: {error}"))?;
    write_private_file(path, &bytes)
}

/// Create `path` with private (mode 0700) permissions on Unix.
fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("could not create config directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("could not secure config directory: {error}"))?;
    }
    Ok(())
}

/// Write `contents` to `path` with mode 0600 on Unix. Used for both
/// JSON config files and key/token secrets.
fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("could not write configuration file: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("could not secure configuration file: {error}"))?;
    }
    file.write_all(contents)
        .map_err(|error| format!("could not write configuration file: {error}"))
}
