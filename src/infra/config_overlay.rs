//! Human-editable TOML configuration overlay.
//!
//! Additive, read-only layer for stable non-secret operator settings.
//! The overlay is edited directly as a file; no new command is required
//! and `config set` / `config clear` continue to operate on the legacy
//! SQLite database only.
//!
//! Effective precedence, highest first:
//!   1. Explicit CLI arguments (`--provider`, `--api-base`, ...).
//!   2. Environment variables (`PHASEGENT_*`).
//!   3. TOML file (`phasegent.toml`).
//!   4. Legacy SQLite settings (`global_setting`, `role_*_config`).
//!   5. Built-in defaults (forgejo fallback, absent optionals).
//!
//! Default path is `<ProjectDirs config_dir>/phasegent.toml` (same
//! directory as `phasegent.sqlite3`). `PHASEGENT_CONFIG_PATH` overrides
//! with an absolute path for test isolation; blank means unset and a
//! relative value is a structured error. A missing file means "no
//! overlay" and falls back to SQLite. A present file with malformed,
//! unknown, or secret/runtime fields fails clearly instead of being
//! silently ignored.
//!
//! Allowed TOML fields (all non-secret):
//!   `default_provider`, `redmine_repository_url`, `index_backend`
//!   (`sqlite`/`postgres`, validated but ignored for backend selection
//!   which remains URL-driven), plus per-role `[roles.<role>]` with
//!   `provider`, `forgejo_api_base`, `forgejo_repository`,
//!   `redmine_api_base`, `redmine_close_status_id`, `gitlab_api_base`.
//! Bearer credentials, role API keys, provisioned identities, timer
//! state, index state, `PHASEGENT_INDEX_PG_URL`, and any
//! credential-bearing URL are never accepted in TOML and fail with a
//! redacted error that names the field but never echoes the value.

use directories::ProjectDirs;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Filename of the TOML overlay inside the ProjectDirs config dir.
pub const CONFIG_FILENAME: &str = "phasegent.toml";
/// Absolute-path override for test isolation.
pub const CONFIG_PATH_ENV: &str = "PHASEGENT_CONFIG_PATH";

/// Typed overlay for stable non-secret settings.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigOverlay {
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub redmine_repository_url: Option<String>,
    #[serde(default)]
    pub index_backend: Option<String>,
    #[serde(default)]
    pub roles: HashMap<String, RoleOverlay>,
}

/// Per-role non-secret endpoint/repository settings.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleOverlay {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub forgejo_api_base: Option<String>,
    #[serde(default)]
    pub forgejo_repository: Option<String>,
    #[serde(default)]
    pub redmine_api_base: Option<String>,
    #[serde(default)]
    pub redmine_close_status_id: Option<u64>,
    #[serde(default)]
    pub gitlab_api_base: Option<String>,
}

/// Default overlay path under the ProjectDirs config directory.
pub fn default_config_path() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("com", "Cloud1ful", "phasegent")
        .ok_or_else(|| "could not resolve phasegent config directory".to_owned())?;
    Ok(dirs.config_dir().join(CONFIG_FILENAME))
}

/// Resolve the overlay path. `PHASEGENT_CONFIG_PATH` must be absolute
/// when non-blank; blank or unset falls back to the default path.
pub fn resolve_config_path() -> Result<PathBuf, String> {
    match std::env::var(CONFIG_PATH_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim().to_owned();
            if trimmed.is_empty() {
                default_config_path()
            } else {
                let path = PathBuf::from(&trimmed);
                if !path.is_absolute() {
                    return Err(format!(
                        "{CONFIG_PATH_ENV} must be an absolute path for test isolation"
                    ));
                }
                Ok(path)
            }
        }
        Err(std::env::VarError::NotPresent) => default_config_path(),
        Err(error) => Err(format!("could not read {CONFIG_PATH_ENV}: {error}")),
    }
}

/// Load the overlay from the resolved path. Missing file means no
/// overlay (`Ok(None)`); present-file errors fail clearly.
pub fn load_overlay() -> Result<Option<ConfigOverlay>, String> {
    let path = resolve_config_path()?;
    load_overlay_from_path(&path)
}

/// Load from an explicit path. Used by tests for path isolation.
pub fn load_overlay_from_path(path: &Path) -> Result<Option<ConfigOverlay>, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(parse_overlay_str(
            &content,
            &path.display().to_string(),
        )?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "could not read TOML config at {}: {error}",
            path.display()
        )),
    }
}

/// Parse and validate TOML content. `origin` names the file for errors
/// and never echoes secret values.
pub fn parse_overlay_str(content: &str, origin: &str) -> Result<ConfigOverlay, String> {
    let table: toml::Table = toml::from_str(content)
        .map_err(|error| format!("could not parse TOML config at {origin}: {error}"))?;
    check_no_forbidden_keys(&table, origin)?;
    let mut overlay: ConfigOverlay = table
        .try_into()
        .map_err(|error| format!("could not parse TOML config at {origin}: {error}"))?;
    overlay.validate(origin)?;
    Ok(overlay)
}

impl ConfigOverlay {
    /// Validated global default provider literal, if present.
    pub fn default_provider_value(&self) -> Option<&str> {
        self.default_provider.as_deref()
    }

    /// Validated repository URL override, if present.
    pub fn redmine_repository_url_value(&self) -> Option<&str> {
        self.redmine_repository_url.as_deref()
    }

    /// Validated legacy backend literal (`sqlite`/`postgres`), if
    /// present. Ignored for backend selection; kept for compatibility.
    #[allow(dead_code)]
    pub fn index_backend_value(&self) -> Option<&str> {
        self.index_backend.as_deref()
    }

    /// Role overlay for `role`, if the file defines `[roles.<role>]`.
    pub fn role_overlay(&self, role: crate::policy::Role) -> Option<&RoleOverlay> {
        self.roles.get(role.as_str())
    }

    fn validate(&mut self, origin: &str) -> Result<(), String> {
        if let Some(value) = self.default_provider.take() {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                return Err(format!(
                    "TOML config at {origin}: field 'default_provider' cannot be empty"
                ));
            }
            match trimmed.as_str() {
                "forgejo" | "redmine" | "gitlab" | "local" => {}
                _ => {
                    return Err(format!(
                        "TOML config at {origin}: invalid default_provider '{trimmed}'; expected forgejo, redmine, gitlab, or local"
                    ));
                }
            }
            self.default_provider = Some(trimmed);
        }
        if let Some(value) = self.redmine_repository_url.take() {
            let validated = validate_repository_url("redmine_repository_url", &value, origin)?;
            self.redmine_repository_url = Some(validated);
        }
        if let Some(value) = self.index_backend.take() {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                return Err(format!(
                    "TOML config at {origin}: field 'index_backend' cannot be empty"
                ));
            }
            let lower = trimmed.to_ascii_lowercase();
            if lower != "sqlite" && lower != "postgres" {
                return Err(format!(
                    "TOML config at {origin}: invalid index_backend '{trimmed}'; expected sqlite or postgres"
                ));
            }
            self.index_backend = Some(lower);
        }
        // Role names must be known; unknown roles fail instead of being ignored.
        for name in self.roles.keys() {
            match name.as_str() {
                "admin" | "orchestrator" | "executor" | "reviewer" | "tester" => {}
                _ => {
                    return Err(format!(
                        "TOML config at {origin}: unknown role '{name}'; expected admin, orchestrator, executor, reviewer, or tester"
                    ));
                }
            }
        }
        for overlay in self.roles.values_mut() {
            overlay.validate(origin)?;
        }
        Ok(())
    }
}

impl RoleOverlay {
    fn validate(&mut self, origin: &str) -> Result<(), String> {
        if let Some(value) = self.provider.take() {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                return Err(format!(
                    "TOML config at {origin}: field 'provider' cannot be empty"
                ));
            }
            match trimmed.as_str() {
                "forgejo" | "redmine" | "gitlab" | "local" => {}
                _ => {
                    return Err(format!(
                        "TOML config at {origin}: invalid provider '{trimmed}'; expected forgejo, redmine, gitlab, or local"
                    ));
                }
            }
            self.provider = Some(trimmed);
        }
        if let Some(value) = self.forgejo_api_base.take() {
            self.forgejo_api_base = Some(validate_api_base("forgejo_api_base", &value, origin)?);
        }
        if let Some(value) = self.forgejo_repository.take() {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                return Err(format!(
                    "TOML config at {origin}: field 'forgejo_repository' cannot be empty"
                ));
            }
            validate_owner_repo(&trimmed).map_err(|error| {
                format!("TOML config at {origin}: invalid forgejo_repository: {error}")
            })?;
            self.forgejo_repository = Some(trimmed);
        }
        if let Some(value) = self.redmine_api_base.take() {
            self.redmine_api_base = Some(validate_api_base("redmine_api_base", &value, origin)?);
        }
        if self.redmine_close_status_id == Some(0) {
            return Err(format!(
                "TOML config at {origin}: field 'redmine_close_status_id' must be greater than zero"
            ));
        }
        if let Some(value) = self.gitlab_api_base.take() {
            self.gitlab_api_base = Some(validate_api_base("gitlab_api_base", &value, origin)?);
        }
        Ok(())
    }
}

fn validate_owner_repo(value: &str) -> Result<(), String> {
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() != 2
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err("repository must use OWNER/REPOSITORY form".to_owned());
    }
    Ok(())
}

fn validate_api_base(key: &str, value: &str, origin: &str) -> Result<String, String> {
    let trimmed = value.trim().to_owned();
    if trimmed.is_empty() {
        return Err(format!(
            "TOML config at {origin}: field '{key}' cannot be empty"
        ));
    }
    let parsed = url::Url::parse(&trimmed).map_err(|error| {
        format!("TOML config at {origin}: field '{key}' is not a valid URL: {error}")
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "TOML config at {origin}: field '{key}' must use http or https"
        ));
    }
    if parsed.host_str().is_none() {
        return Err(format!(
            "TOML config at {origin}: field '{key}' must include a host"
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "TOML config at {origin}: field '{key}' must not contain credentials; keep credentials in env/SQLite"
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!(
            "TOML config at {origin}: field '{key}' must not contain a query or fragment"
        ));
    }
    Ok(trimmed)
}

fn validate_repository_url(key: &str, value: &str, origin: &str) -> Result<String, String> {
    let trimmed = value.trim().to_owned();
    if trimmed.is_empty() {
        return Err(format!(
            "TOML config at {origin}: field '{key}' cannot be empty"
        ));
    }
    // When the value parses as a URL, reject detectable credentials.
    // Scp-style (`git@host:owner/repo.git`) does not parse and is allowed.
    if let Ok(parsed) = url::Url::parse(&trimmed) {
        if parsed.password().is_some() {
            return Err(format!(
                "TOML config at {origin}: field '{key}' must not contain credentials; keep credentials in env/SQLite"
            ));
        }
        let ssh_like = matches!(parsed.scheme(), "ssh" | "git+ssh" | "ssh+git");
        if !ssh_like && !parsed.username().is_empty() {
            return Err(format!(
                "TOML config at {origin}: field '{key}' must not contain credentials; keep credentials in env/SQLite"
            ));
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(format!(
                "TOML config at {origin}: field '{key}' must not contain a query or fragment"
            ));
        }
    }
    Ok(trimmed)
}

fn normalize_key(key: &str) -> String {
    let lower = key.trim().to_ascii_lowercase().replace('-', "_");
    lower
        .strip_prefix("phasegent_")
        .unwrap_or(&lower)
        .to_owned()
}

const FORBIDDEN_EXACT: &[&str] = &[
    "redmine_git_mirror_api_key",
    "git_mirror_api_key",
    "mirror_api_key",
    "index_pg_url",
    "pg_url",
    "database_url",
    "db_url",
    "token",
    "credential",
    "credentials",
    "api_key",
    "private_token",
    "bearer",
    "password",
    "secret",
    "user_id",
    "login",
    "redmine_user",
    "timer",
    "run_id",
    "project_id",
    "redmine_project_id",
    "gitlab_project_id",
    "group_name",
    "group_role",
    "sync_status",
    "sync_error",
    "owner_session_id",
    "owner_call_id",
    "projection_token",
    "projection_claimed_at",
    "activity_id",
    "time_entry_id",
    "elapsed_seconds",
    "rounded_hours",
    "started_at",
    "finished_at",
    "issue_index",
    "index_state",
    "execution_timer_runs",
];

const FORBIDDEN_SUBSTRINGS: &[&str] = &[
    "token",
    "credential",
    "password",
    "secret",
    "bearer",
    "api_key",
    "private_token",
    "pg_url",
    "project_id",
];

fn is_forbidden(normalized: &str) -> bool {
    if FORBIDDEN_EXACT.contains(&normalized) {
        return true;
    }
    // `user_id` / `login` / `timer` / `sync_` / `projection_` / `activity_`
    // never appear in allowed keys, so exact matches suffice for them;
    // substrings below cover compound secret names without risking the
    // allowed `index_backend` (which contains `index` but none of these).
    if matches!(normalized, "user_id" | "login" | "timer")
        || normalized.starts_with("sync_")
        || normalized.starts_with("projection_")
        || normalized.starts_with("activity_")
    {
        return true;
    }
    FORBIDDEN_SUBSTRINGS
        .iter()
        .any(|needle| normalized.contains(needle))
}

fn check_no_forbidden_keys(table: &toml::Table, origin: &str) -> Result<(), String> {
    for (key, value) in table {
        if key == "roles" {
            let roles = value
                .as_table()
                .ok_or_else(|| format!("TOML config at {origin}: field 'roles' must be a table"))?;
            for (role_name, role_value) in roles {
                let role_table = role_value.as_table().ok_or_else(|| {
                    format!("TOML config at {origin}: role '{role_name}' must be a table")
                })?;
                for field in role_table.keys() {
                    let normalized = normalize_key(field);
                    if is_forbidden(&normalized) {
                        return Err(format!(
                            "TOML config at {origin} rejects secret/runtime field '{field}': secrets and runtime state must stay in SQLite/env, not TOML"
                        ));
                    }
                }
            }
            continue;
        }
        let normalized = normalize_key(key);
        if is_forbidden(&normalized) {
            return Err(format!(
                "TOML config at {origin} rejects secret/runtime field '{key}': secrets and runtime state must stay in SQLite/env, not TOML"
            ));
        }
    }
    Ok(())
}
