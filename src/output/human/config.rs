//! The curated configuration view.
//!
//! The view renders the redacted snapshot the command already produced: the
//! presence indicators, the sanitized non-secret values, and the credential
//! fingerprint. A secret value never reaches this view, because the snapshot
//! never carries one.

use serde_json::Value;

use crate::output::context::OutputContext;
use crate::output::render::Lines;

/// The curated `config show` rendering.
pub(crate) fn render(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    if let Some(path) = value.get("database_path").and_then(Value::as_str) {
        lines.field(0, "Database", path);
    }
    let provider = value
        .get("global_default_provider")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    lines.field(0, "Default provider", provider);

    if let Some(settings) = value.get("global_settings").and_then(Value::as_array) {
        lines.blank();
        lines.group(0, "Global settings");
        if settings.is_empty() {
            lines.text(1, "(none)");
        }
        for setting in settings {
            let name = setting
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("(unnamed)");
            let present = setting
                .get("present")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let detail = setting_detail(setting);
            let state = if present { "present" } else { "missing" };
            match detail {
                Some(detail) => lines.field(1, name, &format!("{state} ({detail})")),
                None => lines.field(1, name, state),
            }
        }
    }

    if let Some(roles) = value.get("roles").and_then(Value::as_array) {
        lines.blank();
        lines.group(0, "Roles");
        if roles.is_empty() {
            lines.text(1, "(none)");
        }
        for role in roles {
            lines.text(1, &role_summary(role));
        }
    }
    lines.finish()
}

/// The readable value of one global setting. Only the sanitized and the
/// non-secret values the snapshot carries are used.
fn setting_detail(setting: &Value) -> Option<String> {
    setting
        .get("sanitized_value")
        .and_then(Value::as_str)
        .or_else(|| setting.get("value").and_then(Value::as_str))
        .map(str::to_owned)
}

/// The readable summary of one role scope.
fn role_summary(role: &Value) -> String {
    let name = role
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("(unnamed)");
    let provider = role
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let mut parts = vec![format!("provider {provider}")];
    if let Some(close_status) = role.get("redmine_close_status_id").and_then(Value::as_u64) {
        parts.push(format!("close status {close_status}"));
    }
    let credential = role.get("redmine_credential");
    let present = credential
        .and_then(|credential| credential.get("present"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut credential_text = if present {
        "credential present".to_owned()
    } else {
        "credential missing".to_owned()
    };
    if let Some(fingerprint) = credential
        .and_then(|credential| credential.get("fingerprint"))
        .and_then(Value::as_str)
    {
        credential_text.push_str(&format!(" ({fingerprint})"));
    }
    parts.push(credential_text);
    format!("{name}: {}", parts.join(", "))
}
