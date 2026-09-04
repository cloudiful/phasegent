use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::config::ProviderKind;
use serde_json::Value;
use std::io::{self, Read};

use super::common::{
    update_gitlab_config_field, update_redmine_config_field, update_role_config_field,
};
use super::{ConfigSetOutcome, is_role_scoped_setting, is_secret_setting};

/// Persist a setting value that has already been sourced (from
/// positional arg, stdin content, or interactive prompt). The caller
/// has already decided the source; this helper only validates the
/// trimmed value and writes to SQLite. Secret values are never
/// echoed in errors.
pub fn set_setting_value(
    role: Option<Role>,
    canonical: &str,
    raw_value: &str,
    storage: &Storage,
) -> Result<Value, String> {
    let trimmed = raw_value.trim();
    if trimmed.is_empty() {
        return Err(format!("value for '{canonical}' cannot be empty"));
    }
    if is_secret_setting(canonical) {
        return Err(format!(
            "secret setting '{canonical}' must use --stdin or the interactive prompt"
        ));
    }
    if is_role_scoped_setting(canonical) && role.is_none() {
        return Err(format!("--role is required for setting '{canonical}'"));
    }
    persist_set_value(role, canonical, trimmed, storage)?;
    let outcome = ConfigSetOutcome {
        setting: canonical.to_owned(),
        role: role.map(|r| r.as_str().to_owned()),
        updated: true,
    };
    serde_json::to_value(outcome).map_err(|e| format!("could not encode set outcome: {e}"))
}

/// Helper for tests and stdin dispatch that takes stdin content
/// as a string, trims it, and persists. Rejects empty.
pub fn set_setting_stdin_content(
    role: Option<Role>,
    canonical: &str,
    content: &str,
    storage: &Storage,
) -> Result<Value, String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(format!("value for '{canonical}' cannot be empty"));
    }
    if is_role_scoped_setting(canonical) && role.is_none() {
        return Err(format!("--role is required for setting '{canonical}'"));
    }
    persist_set_value(role, canonical, trimmed, storage)?;
    let outcome = ConfigSetOutcome {
        setting: canonical.to_owned(),
        role: role.map(|r| r.as_str().to_owned()),
        updated: true,
    };
    serde_json::to_value(outcome).map_err(|e| format!("could not encode set outcome: {e}"))
}

/// Top-level dispatch for `config set` that handles secret
/// interactive / stdin sourcing. `value` is the optional positional
/// value; `use_stdin` is the `--stdin` flag. Rejects direct secret
/// values and empty values, and avoids echoing secrets.
pub fn dispatch_set(
    role: Option<Role>,
    canonical: &str,
    value: Option<&str>,
    use_stdin: bool,
    storage: &Storage,
) -> Result<Value, String> {
    if is_secret_setting(canonical) {
        if value.is_some() {
            return Err(format!(
                "secret setting '{canonical}' does not accept a direct value; use --stdin or interactive prompt"
            ));
        }
        if use_stdin {
            let content = read_stdin_trimmed()?;
            if content.trim().is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            set_setting_stdin_content(role, canonical, &content, storage)
        } else {
            let content = prompt_secret(canonical)?;
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            persist_set_value(role, canonical, trimmed, storage)?;
            let outcome = ConfigSetOutcome {
                setting: canonical.to_owned(),
                role: role.map(|r| r.as_str().to_owned()),
                updated: true,
            };
            serde_json::to_value(outcome).map_err(|e| format!("could not encode set outcome: {e}"))
        }
    } else {
        if use_stdin && value.is_some() {
            return Err("cannot provide both a value and --stdin".to_owned());
        }
        if use_stdin {
            let content = read_stdin_trimmed()?;
            let trimmed = content.trim().to_owned();
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            set_setting_value(role, canonical, &trimmed, storage)
        } else {
            let raw = value
                .ok_or_else(|| format!("config set {canonical} requires a value or --stdin"))?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            set_setting_value(role, canonical, trimmed, storage)
        }
    }
}

fn persist_set_value(
    role: Option<Role>,
    canonical: &str,
    value: &str,
    storage: &Storage,
) -> Result<(), String> {
    let trimmed = value.trim();
    match canonical {
        "PHASEGENT_PROVIDER" => {
            let role = role.expect("role required checked above");
            let kind: ProviderKind = trimmed
                .parse()
                .map_err(|e: String| format!("invalid provider '{trimmed}': {e}"))?;
            storage.update_provider(role, kind.as_str())?;
        }
        "PHASEGENT_API_BASE" => {
            let role = role.expect("role required");
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            update_role_config_field(storage, role, |c| {
                c.api_base = Some(trimmed.to_owned());
            })?;
            update_redmine_config_field(storage, role, |c| {
                c.api_base = Some(trimmed.to_owned());
            })?;
            update_gitlab_config_field(storage, role, |c| {
                c.api_base = Some(trimmed.to_owned());
            })?;
        }
        "PHASEGENT_REPOSITORY" => {
            let role = role.expect("role required");
            update_role_config_field(storage, role, |c| {
                c.repository = Some(trimmed.to_owned());
            })?;
        }
        "PHASEGENT_REDMINE_API_BASE" => {
            let role = role.expect("role required");
            update_redmine_config_field(storage, role, |c| {
                c.api_base = Some(trimmed.to_owned());
            })?;
        }
        "PHASEGENT_REDMINE_CLOSE_STATUS_ID" => {
            let role = role.expect("role required");
            let parsed = trimmed
                .parse::<u64>()
                .map_err(|_| format!("could not parse {canonical} '{trimmed}': must be numeric"))?;
            if parsed == 0 {
                return Err(format!("{canonical} must be greater than zero"));
            }
            update_redmine_config_field(storage, role, |c| {
                c.close_status_id = Some(parsed);
            })?;
        }
        "PHASEGENT_GITLAB_API_BASE" => {
            let role = role.expect("role required");
            update_gitlab_config_field(storage, role, |c| {
                c.api_base = Some(trimmed.to_owned());
            })?;
        }
        "PHASEGENT_CLOSE_STATUS_ID" => {
            let role = role.expect("role required");
            let parsed = trimmed
                .parse::<u64>()
                .map_err(|_| format!("could not parse {canonical} '{trimmed}': must be numeric"))?;
            if parsed == 0 {
                return Err(format!("{canonical} must be greater than zero"));
            }
            update_redmine_config_field(storage, role, |c| {
                c.close_status_id = Some(parsed);
            })?;
        }
        "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY" => {
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            storage.save_global_setting(canonical, trimmed)?;
        }
        "PHASEGENT_REDMINE_REPOSITORY_URL" => {
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            storage.save_global_setting(canonical, trimmed)?;
        }
        "PHASEGENT_DEFAULT_PROVIDER" => {
            let kind: ProviderKind = trimmed
                .parse()
                .map_err(|e: String| format!("invalid provider '{trimmed}': {e}"))?;
            storage.save_global_setting(canonical, kind.as_str())?;
        }
        "PHASEGENT_INDEX_BACKEND" => {
            let lower = trimmed.to_ascii_lowercase();
            if lower != "sqlite" && lower != "postgres" {
                return Err(format!(
                    "invalid PHASEGENT_INDEX_BACKEND '{trimmed}'; expected sqlite or postgres"
                ));
            }
            storage.save_global_setting(canonical, &lower)?;
        }
        "PHASEGENT_INDEX_PG_URL" => {
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            storage.save_global_setting(canonical, trimmed)?;
        }
        _ => return Err(format!("unknown setting '{canonical}'")),
    }
    Ok(())
}

fn read_stdin_trimmed() -> Result<String, String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("could not read from stdin: {e}"))?;
    Ok(input.trim().to_owned())
}

fn prompt_secret(canonical: &str) -> Result<String, String> {
    let prompt = format!("{canonical}: ");
    rpassword::prompt_password(prompt)
        .map_err(|e| format!("could not read secret securely: {e}"))
        .map(|v| v.trim().to_owned())
}
