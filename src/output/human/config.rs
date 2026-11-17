//! The curated configuration view.
//!
//! The view renders the redacted snapshot the command already produced: the
//! presence indicators, the sanitized non-secret values, and the credential
//! fingerprint. A secret value never reaches this view, because the snapshot
//! never carries one.
//!
//! The settings are rendered under the section that owns them, so the view
//! mirrors the typed configuration tree instead of a flat list of names.

use serde_json::Value;

use crate::output::context::OutputContext;
use crate::output::render::Lines;

/// The curated `config show` rendering.
pub(crate) fn render(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    if let Some(path) = value.get("config_path").and_then(Value::as_str) {
        lines.field(0, "Configuration file", path);
    }
    let provider = value
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    lines.field(0, "Default provider", provider);

    if let Some(redmine) = value.get("redmine") {
        lines.blank();
        lines.group(0, "Redmine");
        lines.field(1, "apiBase", &setting_state(redmine, "apiBase"));
        lines.field(1, "repositoryUrl", &setting_state(redmine, "repositoryUrl"));
        if let Some(git_mirror) = redmine.get("gitMirror") {
            lines.field(1, "gitMirror.apiKey", &setting_state(git_mirror, "apiKey"));
        }
    }

    if let Some(index) = value.get("index").and_then(|index| index.get("postgres")) {
        lines.blank();
        lines.group(0, "Index");
        lines.field(1, "postgres.url", &setting_state(index, "url"));
    }

    if let Some(notifications) = value.get("notifications") {
        lines.blank();
        lines.group(0, "Notifications");
        lines.field(1, "enabled", &setting_state(notifications, "enabled"));
        lines.field(1, "channel", &setting_state(notifications, "channel"));
        if let Some(ntfy) = notifications.get("ntfy") {
            lines.field(1, "ntfy.baseUrl", &setting_state(ntfy, "baseUrl"));
            lines.field(1, "ntfy.topic", &setting_state(ntfy, "topic"));
            lines.field(1, "ntfy.token", &setting_state(ntfy, "token"));
        }
        if let Some(webhook) = notifications.get("webhook") {
            lines.field(1, "webhook.url", &setting_state(webhook, "url"));
            lines.field(1, "webhook.token", &setting_state(webhook, "token"));
        }
        if let Some(dingtalk) = notifications.get("dingtalk") {
            lines.field(
                1,
                "dingtalk.webhookUrl",
                &setting_state(dingtalk, "webhookUrl"),
            );
            lines.field(1, "dingtalk.secret", &setting_state(dingtalk, "secret"));
            lines.field(1, "dingtalk.keywords", &setting_state(dingtalk, "keywords"));
            lines.field(
                1,
                "dingtalk.messageType",
                &setting_state(dingtalk, "messageType"),
            );
        }
        if let Some(email) = notifications.get("email") {
            if let Some(smtp) = email.get("smtp") {
                lines.field(1, "email.smtp.host", &setting_state(smtp, "host"));
                lines.field(1, "email.smtp.port", &setting_state(smtp, "port"));
                lines.field(1, "email.smtp.tls", &setting_state(smtp, "tls"));
            }
            lines.field(1, "email.username", &setting_state(email, "username"));
            lines.field(1, "email.password", &setting_state(email, "password"));
            lines.field(1, "email.from", &setting_state(email, "from"));
            lines.field(1, "email.to", &setting_state(email, "to"));
            lines.field(1, "email.replyTo", &setting_state(email, "replyTo"));
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

/// The readable state of one setting. Only the sanitized and the
/// non-secret values the snapshot carries are used.
fn setting_state(section: &Value, name: &str) -> String {
    let Some(setting) = section.get(name) else {
        return "missing".to_owned();
    };
    let present = setting
        .get("present")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !present {
        return "missing".to_owned();
    }
    match setting_detail(setting) {
        Some(detail) => format!("present ({detail})"),
        None => "present".to_owned(),
    }
}

/// The readable value of one setting.
fn setting_detail(setting: &Value) -> Option<String> {
    if let Some(sanitized) = setting.get("sanitized_value").and_then(Value::as_str) {
        return Some(sanitized.to_owned());
    }
    match setting.get("value") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Bool(value)) => Some(value.to_string()),
        Some(Value::Number(value)) => Some(value.to_string()),
        Some(Value::Array(values)) => Some(
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<&str>>()
                .join(", "),
        ),
        _ => None,
    }
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
