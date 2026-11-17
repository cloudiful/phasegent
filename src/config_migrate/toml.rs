//! Migration-only decoder for the legacy `phasegent.toml` overlay.
//!
//! The overlay is no longer read at runtime: the JSONC configuration file
//! is the only runtime source. This decoder exists so the human-operated
//! `admin config migrate` command can fold an explicit TOML file over the
//! legacy SQLite values with the old precedence.
//!
//! The decoder never reads the ambient environment, never writes, and
//! never resolves a path on its own: the operator names the file
//! explicitly with `--from-toml`.

use crate::infra::config_file::{LegacyTomlOverlay, LegacyTomlRole};
use std::collections::BTreeMap;
use std::path::Path;

/// The top-level fields of the legacy TOML overlay. A field outside this
/// list is refused; the offending name is never echoed back.
const OVERLAY_FIELDS: [&str; 4] = [
    "default_provider",
    "redmine_api_base",
    "redmine_repository_url",
    "index_backend",
];

/// The fields of a role entry of the legacy TOML overlay.
const ROLE_FIELDS: [&str; 3] = ["provider", "redmine_api_base", "redmine_close_status_id"];

/// Decode the TOML overlay at `path`. A missing file is an explicit error:
/// the operator named the file, so its absence is a mistake, never a
/// silent "no overlay".
pub fn load_overlay(path: &Path) -> Result<LegacyTomlOverlay, String> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "could not read the TOML configuration at {}: {error}",
            path.display()
        )
    })?;
    parse_overlay(&content, &path.display().to_string())
}

/// Decode TOML overlay content. `origin` names the file for errors.
///
/// No parser diagnostic is forwarded: a TOML error message can quote an
/// unknown key or an invalid value, and either can be an accidentally
/// pasted secret. A syntax error is therefore reported as a fixed category
/// with the position of the fault, and a rejected field is reported as a
/// fixed category without the name or the value of the offending field.
pub fn parse_overlay(content: &str, origin: &str) -> Result<LegacyTomlOverlay, String> {
    let table: toml::Table = toml::from_str(content).map_err(|error| {
        format!(
            "could not parse TOML config at {origin}: {}",
            syntax_diagnostic(content, &error)
        )
    })?;
    check_no_forbidden_keys(&table, origin)?;
    build_overlay(&table, origin)
}

/// The fixed diagnostic of a TOML syntax error: the category and the
/// position of the fault, never the parser message and never the source.
fn syntax_diagnostic(content: &str, error: &toml::de::Error) -> String {
    let Some(span) = error.span() else {
        return "invalid TOML syntax".to_owned();
    };
    let (line, column) = position_of(content, span.start);
    format!("invalid TOML syntax at line {line} column {column}")
}

/// The one-based line and column of a byte offset.
fn position_of(content: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(content.len());
    let before = &content[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map(|index| before[index + 1..].chars().count() + 1)
        .unwrap_or_else(|| before.chars().count() + 1);
    (line, column)
}

/// Map the parsed table onto the overlay. Every rejection is a fixed
/// category: an unknown field, an unknown role, or a wrong value type.
fn build_overlay(table: &toml::Table, origin: &str) -> Result<LegacyTomlOverlay, String> {
    let mut default_provider = None;
    let mut redmine_api_base = None;
    let mut redmine_repository_url = None;
    let mut index_backend = None;
    let mut roles = BTreeMap::new();

    for (key, value) in table {
        if key == "roles" {
            let role_table = value
                .as_table()
                .ok_or_else(|| format!("TOML config at {origin}: field 'roles' must be a table"))?;
            for (name, entry) in role_table {
                roles.insert(name.clone(), build_role(name, entry, origin)?);
            }
            continue;
        }
        if !OVERLAY_FIELDS.contains(&key.as_str()) {
            return Err(format!(
                "TOML config at {origin}: unknown field; expected one of the known overlay fields"
            ));
        }
        let text = string_field(key, value, origin)?;
        match key.as_str() {
            "default_provider" => {
                default_provider = Some(validate_provider("default_provider", text, origin)?);
            }
            "redmine_api_base" => {
                redmine_api_base = Some(validate_api_base("redmine_api_base", &text, origin)?);
            }
            "redmine_repository_url" => {
                redmine_repository_url = Some(validate_repository_url(
                    "redmine_repository_url",
                    &text,
                    origin,
                )?);
            }
            "index_backend" => {
                index_backend = Some(validate_index_backend(&text, origin)?);
            }
            _ => unreachable!("the field list and the mapping agree"),
        }
    }

    Ok(LegacyTomlOverlay {
        default_provider,
        redmine_api_base,
        redmine_repository_url,
        index_backend,
        roles,
    })
}

/// Map one role entry onto the overlay.
fn build_role(name: &str, entry: &toml::Value, origin: &str) -> Result<LegacyTomlRole, String> {
    if !matches!(
        name,
        "admin" | "orchestrator" | "executor" | "reviewer" | "explore"
    ) {
        return Err(format!(
            "TOML config at {origin}: unknown role; expected admin, orchestrator, executor, reviewer, or explore"
        ));
    }
    let role_table = entry
        .as_table()
        .ok_or_else(|| format!("TOML config at {origin}: every role entry must be a table"))?;
    let mut provider = None;
    let mut redmine_api_base = None;
    let mut redmine_close_status_id = None;
    for (field, value) in role_table {
        if !ROLE_FIELDS.contains(&field.as_str()) {
            return Err(format!(
                "TOML config at {origin}: unknown field in a role entry; expected one of the known role fields"
            ));
        }
        match field.as_str() {
            "provider" => {
                provider = Some(validate_provider(
                    "provider",
                    string_field(field, value, origin)?,
                    origin,
                )?)
            }
            "redmine_api_base" => {
                redmine_api_base = Some(validate_api_base(
                    "redmine_api_base",
                    &string_field(field, value, origin)?,
                    origin,
                )?);
            }
            "redmine_close_status_id" => {
                let number = value.as_integer().ok_or_else(|| {
                    format!(
                        "TOML config at {origin}: field 'redmine_close_status_id' must be an integer"
                    )
                })?;
                if number <= 0 {
                    return Err(format!(
                        "TOML config at {origin}: field 'redmine_close_status_id' must be greater than zero"
                    ));
                }
                redmine_close_status_id = Some(number as u64);
            }
            _ => unreachable!("the field list and the mapping agree"),
        }
    }
    Ok(LegacyTomlRole {
        provider,
        redmine_api_base,
        redmine_close_status_id,
    })
}

/// The string value of a known field. The field name is one of this
/// module's own names, so it is safe to name; the value never is.
fn string_field(key: &str, value: &toml::Value, origin: &str) -> Result<String, String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("TOML config at {origin}: field '{key}' must be a string"))
}

fn validate_provider(key: &str, value: String, origin: &str) -> Result<String, String> {
    let trimmed = value.trim().to_owned();
    if trimmed.is_empty() {
        return Err(format!(
            "TOML config at {origin}: field '{key}' cannot be empty"
        ));
    }
    trimmed
        .parse::<crate::providers::config::ProviderKind>()
        .map_err(|_| format!("TOML config at {origin}: invalid {key}"))?;
    Ok(trimmed)
}

fn validate_index_backend(value: &str, origin: &str) -> Result<String, String> {
    let trimmed = value.trim().to_owned();
    if trimmed.is_empty() {
        return Err(format!(
            "TOML config at {origin}: field 'index_backend' cannot be empty"
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower != "sqlite" && lower != "postgres" {
        return Err(format!(
            "TOML config at {origin}: invalid index_backend; expected sqlite or postgres"
        ));
    }
    Ok(lower)
}

fn validate_api_base(key: &str, value: &str, origin: &str) -> Result<String, String> {
    let trimmed = value.trim().to_owned();
    if trimmed.is_empty() {
        return Err(format!(
            "TOML config at {origin}: field '{key}' cannot be empty"
        ));
    }
    let parsed = url::Url::parse(&trimmed)
        .map_err(|_| format!("TOML config at {origin}: field '{key}' is not a valid URL"))?;
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
            "TOML config at {origin}: field '{key}' must not contain credentials; \
             credentials belong in the JSONC configuration file"
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
                "TOML config at {origin}: field '{key}' must not contain credentials; \
                 credentials belong in the JSONC configuration file"
            ));
        }
        let ssh_like = matches!(parsed.scheme(), "ssh" | "git+ssh" | "ssh+git");
        if !ssh_like && !parsed.username().is_empty() {
            return Err(format!(
                "TOML config at {origin}: field '{key}' must not contain credentials; \
                 credentials belong in the JSONC configuration file"
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

/// Reject secret and runtime fields before the overlay is mapped, so a
/// value that the JSONC model cannot represent is reported instead of
/// being dropped silently.
fn check_no_forbidden_keys(table: &toml::Table, origin: &str) -> Result<(), String> {
    for (key, value) in table {
        if key == "roles" {
            let roles = value
                .as_table()
                .ok_or_else(|| format!("TOML config at {origin}: field 'roles' must be a table"))?;
            for (_role_name, role_value) in roles {
                let role_table = role_value.as_table().ok_or_else(|| {
                    format!("TOML config at {origin}: every role entry must be a table")
                })?;
                for field in role_table.keys() {
                    let normalized = normalize_key(field);
                    if is_forbidden(&normalized) {
                        return Err(format!(
                            "TOML config at {origin} rejects a secret/runtime field: \
                             secrets and runtime state must stay in the JSONC configuration file"
                        ));
                    }
                }
            }
            continue;
        }
        let normalized = normalize_key(key);
        if is_forbidden(&normalized) {
            return Err(format!(
                "TOML config at {origin} rejects a secret/runtime field: \
                 secrets and runtime state must stay in the JSONC configuration file"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_overlay_decodes_into_the_legacy_mapping() {
        let overlay = parse_overlay(
            r#"
            default_provider = "forgejo"
            redmine_api_base = "https://redmine.example"
            redmine_repository_url = "ssh://git@example/owner/repo.git"
            index_backend = "POSTGRES"

            [roles.executor]
            provider = "gitlab"
            redmine_api_base = "https://role-redmine.example"
            redmine_close_status_id = 9
            "#,
            "overlay.toml",
        )
        .unwrap();
        assert_eq!(overlay.default_provider.as_deref(), Some("forgejo"));
        assert_eq!(
            overlay.redmine_api_base.as_deref(),
            Some("https://redmine.example")
        );
        assert_eq!(
            overlay.redmine_repository_url.as_deref(),
            Some("ssh://git@example/owner/repo.git")
        );
        assert_eq!(overlay.index_backend.as_deref(), Some("postgres"));
        let role = overlay.roles.get("executor").unwrap();
        assert_eq!(role.provider.as_deref(), Some("gitlab"));
        assert_eq!(
            role.redmine_api_base.as_deref(),
            Some("https://role-redmine.example")
        );
        assert_eq!(role.redmine_close_status_id, Some(9));
    }

    #[test]
    fn unknown_keys_roles_and_invalid_values_are_refused() {
        for (text, expected) in [
            ("surprise = true\n", "unknown field"),
            ("[roles.intruder]\nprovider = \"redmine\"\n", "unknown role"),
            (
                "default_provider = \"bitbucket\"\n",
                "invalid default_provider",
            ),
            ("index_backend = \"mysql\"\n", "invalid index_backend"),
            (
                "[roles.executor]\nredmine_close_status_id = 0\n",
                "greater than zero",
            ),
        ] {
            let error = parse_overlay(text, "overlay.toml").unwrap_err();
            assert!(
                error.contains(expected),
                "expected '{expected}' in '{error}' for {text}"
            );
        }
    }

    #[test]
    fn secret_and_runtime_fields_are_refused_without_echoing_the_value() {
        let error = parse_overlay(
            "redmine_git_mirror_api_key = \"super-secret-token\"\n",
            "overlay.toml",
        )
        .unwrap_err();
        assert!(error.contains("rejects a secret/runtime field"));
        assert!(!error.contains("super-secret-token"));

        let error = parse_overlay(
            "[roles.executor]\ncredential = \"super-secret-token\"\n",
            "overlay.toml",
        )
        .unwrap_err();
        assert!(error.contains("rejects a secret/runtime field"));
        assert!(!error.contains("super-secret-token"));
    }
}
