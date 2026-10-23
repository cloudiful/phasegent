//! The curated doctor view.
//!
//! The view separates the healthy checks from the warnings so a reader sees
//! what is ready and what still needs attention. Every fact comes from the
//! report the command already produced.

use serde_json::Value;

use crate::output::context::OutputContext;
use crate::output::render::Lines;

/// The curated `doctor` rendering.
pub(crate) fn render(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    let mut healthy: Vec<(String, String)> = Vec::new();
    let mut warnings: Vec<(String, String)> = Vec::new();

    if let Some(path) = value.get("database_path").and_then(Value::as_str) {
        healthy.push(("database".to_owned(), path.to_owned()));
    }
    match value.get("global_default_provider").and_then(Value::as_str) {
        Some(provider) => healthy.push(("default provider".to_owned(), provider.to_owned())),
        None => warnings.push(("default provider".to_owned(), "not set".to_owned())),
    }
    if let Some(index) = value.get("index") {
        let backend = index
            .get("backend")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        match index.get("sqlite_path").and_then(Value::as_str) {
            Some(path) => {
                let exists = index
                    .get("sqlite_exists")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if exists {
                    healthy.push(("issue index".to_owned(), format!("{backend} at {path}")));
                } else {
                    warnings.push((
                        "issue index".to_owned(),
                        format!("{backend} is not created yet ({path})"),
                    ));
                }
            }
            None => match index.get("pg_url_present").and_then(Value::as_bool) {
                Some(true) => {
                    healthy.push(("issue index".to_owned(), format!("{backend} is configured")))
                }
                _ => warnings.push((
                    "issue index".to_owned(),
                    format!("{backend} has no configured URL"),
                )),
            },
        }
    }

    if !healthy.is_empty() {
        lines.group(0, "Healthy");
        for (label, detail) in &healthy {
            lines.field(1, label, detail);
        }
    }
    if !warnings.is_empty() {
        if !healthy.is_empty() {
            lines.blank();
        }
        lines.group(0, "Warnings");
        for (label, detail) in &warnings {
            lines.field(1, label, detail);
        }
    }
    if let Some(roles) = value.get("roles").and_then(Value::as_array)
        && !roles.is_empty()
    {
        lines.blank();
        lines.group(0, "Roles");
        for role in roles {
            let name = role
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("(unnamed)");
            let present = role
                .get("redmine_credential")
                .and_then(|credential| credential.get("present"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let state = if present {
                "credential present"
            } else {
                "credential missing"
            };
            lines.text(1, &format!("{name}: {state}"));
        }
    }
    lines.finish()
}
