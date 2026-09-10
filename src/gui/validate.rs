//! Pure input validation and redaction helpers for the GUI boundary.
//!
//! Everything here is side-effect free and tested without network or
//! real credentials. Bounds keep IPC payloads small; errors never
//! echo secret values or raw URLs with userinfo.

const DEFAULT_TASK_LIMIT: usize = 20;
const MAX_TASK_LIMIT: usize = 50;
const MAX_SETTING_VALUE_LEN: usize = 4096;
const MAX_CREDENTIAL_LEN: usize = 8192;
const MAX_ERROR_CHARS: usize = 300;
const MAX_TITLE_CHARS: usize = 500;

/// Bound an error/message string: single line, truncated, no control
/// chars. Never appends secret values; callers must not interpolate
/// credentials or raw URLs with userinfo.
pub fn bound_message(raw: impl AsRef<str>) -> String {
    let raw = raw.as_ref();
    let mut text = raw.trim().replace(['\n', '\r'], " ");
    while text.contains("  ") {
        text = text.replace("  ", " ");
    }
    let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
    if cleaned.len() <= MAX_ERROR_CHARS {
        return cleaned;
    }
    let mut end = MAX_ERROR_CHARS;
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].to_owned()
}

pub(crate) fn now_fetched_at() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn sanitize_optional_url(value: Option<String>) -> Option<String> {
    value.map(|v| crate::config_snapshot::sanitize_url(&v))
}

pub(crate) fn bound_title(raw: &str) -> String {
    let single = raw.trim().replace(['\n', '\r'], " ");
    let cleaned: String = single.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= MAX_TITLE_CHARS {
        return trimmed.to_owned();
    }
    trimmed.chars().take(MAX_TITLE_CHARS).collect()
}

pub(crate) fn parse_role_with_default(input: Option<&str>) -> Result<crate::policy::Role, String> {
    match input.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(crate::policy::Role::Executor),
        Some(raw) => raw
            .to_ascii_lowercase()
            .parse::<crate::policy::Role>()
            .map_err(bound_message),
    }
}

pub(crate) fn parse_role_optional(
    input: Option<&str>,
) -> Result<Option<crate::policy::Role>, String> {
    match input.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(raw) => raw
            .to_ascii_lowercase()
            .parse::<crate::policy::Role>()
            .map(Some)
            .map_err(bound_message),
    }
}

pub(crate) fn parse_provider_optional(
    input: Option<&str>,
) -> Result<Option<crate::providers::config::ProviderKind>, String> {
    match input.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(raw) => raw
            .to_ascii_lowercase()
            .parse::<crate::providers::config::ProviderKind>()
            .map(Some)
            .map_err(bound_message),
    }
}

pub(crate) fn parse_provider_required(
    input: &str,
) -> Result<crate::providers::config::ProviderKind, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("provider cannot be empty".to_owned());
    }
    trimmed
        .to_ascii_lowercase()
        .parse::<crate::providers::config::ProviderKind>()
        .map_err(bound_message)
}

pub(crate) fn parse_role_required(input: &str) -> Result<crate::policy::Role, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("role cannot be empty".to_owned());
    }
    trimmed
        .to_ascii_lowercase()
        .parse::<crate::policy::Role>()
        .map_err(bound_message)
}

pub fn validate_task_limit(input: Option<usize>) -> Result<usize, String> {
    match input {
        None => Ok(DEFAULT_TASK_LIMIT),
        Some(0) => Err("task limit must be between 1 and 50".to_owned()),
        Some(n) if n > MAX_TASK_LIMIT => {
            Err(format!("task limit must be between 1 and {MAX_TASK_LIMIT}"))
        }
        Some(n) => Ok(n),
    }
}

pub fn validate_task_state(input: Option<&str>) -> Result<String, String> {
    match input.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok("open".to_owned()),
        Some(raw) => {
            let lower = raw.to_ascii_lowercase();
            if matches!(lower.as_str(), "open" | "closed" | "all") {
                Ok(lower)
            } else {
                Err("task state must be open, closed, or all".to_owned())
            }
        }
    }
}

pub fn canonical_non_secret_setting(input: &str) -> Result<&'static str, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("setting cannot be empty".to_owned());
    }
    let canonical = crate::config_write::canonical_setting_name(trimmed)
        .ok_or_else(|| bound_message(format!("unknown setting '{trimmed}'")))?;
    if crate::config_write::is_secret_setting(canonical) {
        return Err(format!(
            "secret setting '{canonical}' must use the credential path"
        ));
    }
    Ok(canonical)
}

pub fn validate_setting_value(canonical: &str, value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("value for '{canonical}' cannot be empty"));
    }
    if trimmed.chars().count() > MAX_SETTING_VALUE_LEN {
        return Err(format!(
            "value for '{canonical}' must be at most {MAX_SETTING_VALUE_LEN} characters"
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(format!(
            "value for '{canonical}' must not contain control characters"
        ));
    }
    Ok(trimmed.to_owned())
}

pub fn validate_credential_value(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("credential cannot be empty".to_owned());
    }
    if trimmed.chars().count() > MAX_CREDENTIAL_LEN {
        return Err(format!(
            "credential must be at most {MAX_CREDENTIAL_LEN} characters"
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("credential must not contain control characters".to_owned());
    }
    Ok(trimmed.to_owned())
}
