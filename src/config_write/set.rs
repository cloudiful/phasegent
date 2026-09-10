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
        // Legacy compatibility only: validated and persisted so existing
        // scripts keep working, but ignored for backend selection (URL
        // presence alone selects PostgreSQL; absence selects SQLite).
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
        "PHASEGENT_NOTIFY_ENABLED" | "PHASEGENT_WORKTREE_AUTO" => {
            let normalised = crate::config_write::parse_bool_literal(trimmed).ok_or_else(|| {
                format!("invalid {canonical} '{trimmed}'; expected true or false")
            })?;
            storage.save_global_setting(canonical, if normalised { "true" } else { "false" })?;
        }
        "PHASEGENT_NOTIFY_CHANNEL" => {
            let lower = trimmed.to_ascii_lowercase();
            if !matches!(lower.as_str(), "ntfy" | "webhook" | "dingtalk" | "email") {
                return Err(format!(
                    "invalid PHASEGENT_NOTIFY_CHANNEL '{trimmed}'; expected ntfy, webhook, dingtalk, or email"
                ));
            }
            storage.save_global_setting(canonical, &lower)?;
        }
        "PHASEGENT_NOTIFY_NTFY_BASE_URL" => {
            validate_http_base_url(canonical, trimmed)?;
            storage.save_global_setting(canonical, trimmed)?;
        }
        "PHASEGENT_NOTIFY_NTFY_TOPIC" => {
            validate_ntfy_topic(trimmed)?;
            storage.save_global_setting(canonical, trimmed)?;
        }
        "PHASEGENT_NOTIFY_NTFY_TOKEN"
        | "PHASEGENT_NOTIFY_WEBHOOK_TOKEN"
        | "PHASEGENT_NOTIFY_DINGTALK_SECRET"
        | "PHASEGENT_NOTIFY_EMAIL_PASSWORD" => {
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            if trimmed.chars().count() > 8192 {
                return Err(format!("value for '{canonical}' is too long"));
            }
            storage.save_global_setting(canonical, trimmed)?;
        }
        "PHASEGENT_NOTIFY_WEBHOOK_URL" => {
            validate_webhook_url(canonical, trimmed)?;
            storage.save_global_setting(canonical, trimmed)?;
        }
        "PHASEGENT_NOTIFY_DINGTALK_WEBHOOK_URL" => {
            validate_webhook_url(canonical, trimmed)?;
            storage.save_global_setting(canonical, trimmed)?;
        }
        "PHASEGENT_NOTIFY_DINGTALK_KEYWORDS" => {
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            if trimmed.chars().count() > 1024 {
                return Err(format!("value for '{canonical}' is too long"));
            }
            storage.save_global_setting(canonical, trimmed)?;
        }
        "PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE" => {
            let lower = trimmed.to_ascii_lowercase();
            if lower != "text" && lower != "markdown" {
                return Err(format!(
                    "invalid {canonical} '{trimmed}'; expected text or markdown"
                ));
            }
            storage.save_global_setting(canonical, &lower)?;
        }
        "PHASEGENT_NOTIFY_EMAIL_SMTP_HOST" => {
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            if trimmed.chars().any(char::is_control) || trimmed.contains(char::is_whitespace) {
                return Err(format!("value for '{canonical}' must be a bare hostname"));
            }
            storage.save_global_setting(canonical, trimmed)?;
        }
        "PHASEGENT_NOTIFY_EMAIL_SMTP_PORT" => {
            let port: u16 = trimmed
                .parse()
                .map_err(|_| format!("could not parse {canonical} '{trimmed}': must be 1-65535"))?;
            if port == 0 {
                return Err(format!("{canonical} must be greater than zero"));
            }
            storage.save_global_setting(canonical, &port.to_string())?;
        }
        "PHASEGENT_NOTIFY_EMAIL_TLS" => {
            let lower = trimmed.to_ascii_lowercase();
            if !matches!(lower.as_str(), "implicit" | "starttls" | "plain") {
                return Err(format!(
                    "invalid {canonical} '{trimmed}'; expected implicit, starttls, or plain"
                ));
            }
            storage.save_global_setting(canonical, &lower)?;
        }
        "PHASEGENT_NOTIFY_EMAIL_USERNAME"
        | "PHASEGENT_NOTIFY_EMAIL_FROM"
        | "PHASEGENT_NOTIFY_EMAIL_TO"
        | "PHASEGENT_NOTIFY_EMAIL_REPLY_TO" => {
            if trimmed.is_empty() {
                return Err(format!("value for '{canonical}' cannot be empty"));
            }
            if !trimmed.contains('@') {
                return Err(format!("value for '{canonical}' must contain '@'"));
            }
            if trimmed.chars().count() > 2048 {
                return Err(format!("value for '{canonical}' is too long"));
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

/// Validate an ntfy server base URL: http/https with a host and no
/// credentials, query, or fragment. The topic is appended as a path
/// segment at send time, so the base must be a clean origin.
fn validate_http_base_url(canonical: &str, value: &str) -> Result<(), String> {
    let parsed = url::Url::parse(value)
        .map_err(|e| format!("value for '{canonical}' is not a valid URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("value for '{canonical}' must use http or https"));
    }
    if parsed.host_str().is_none() {
        return Err(format!("value for '{canonical}' must include a host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "value for '{canonical}' must not contain credentials"
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!(
            "value for '{canonical}' must not contain a query or fragment"
        ));
    }
    Ok(())
}

/// Validate a webhook-style URL: http/https with a host and no
/// userinfo. Query and fragment are allowed because signed webhook
/// URLs commonly carry them; secrets still belong in the token
/// setting, never in the URL.
fn validate_webhook_url(canonical: &str, value: &str) -> Result<(), String> {
    let parsed = url::Url::parse(value)
        .map_err(|e| format!("value for '{canonical}' is not a valid URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("value for '{canonical}' must use http or https"));
    }
    if parsed.host_str().is_none() {
        return Err(format!("value for '{canonical}' must include a host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "value for '{canonical}' must not contain credentials"
        ));
    }
    Ok(())
}

/// Validate an ntfy topic: non-empty, no whitespace/control/slash,
/// bounded length. The value becomes a single path segment.
fn validate_ntfy_topic(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("value for 'PHASEGENT_NOTIFY_NTFY_TOPIC' cannot be empty".to_owned());
    }
    if value.chars().count() > 256 {
        return Err("value for 'PHASEGENT_NOTIFY_NTFY_TOPIC' is too long".to_owned());
    }
    if value
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '/')
    {
        return Err(
            "value for 'PHASEGENT_NOTIFY_NTFY_TOPIC' must not contain whitespace or '/'".to_owned(),
        );
    }
    Ok(())
}

fn prompt_secret(canonical: &str) -> Result<String, String> {
    let prompt = format!("{canonical}: ");
    rpassword::prompt_password(prompt)
        .map_err(|e| format!("could not read secret securely: {e}"))
        .map(|v| v.trim().to_owned())
}
