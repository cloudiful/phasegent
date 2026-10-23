//! The curated status views: the next-status report, the transition result,
//! and the status catalogue.

use serde_json::Value;

use crate::output::context::OutputContext;
use crate::output::render::Lines;

/// The curated `status next` rendering.
pub(crate) fn render_next(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    lines.heading("Status report");
    lines.blank();
    if let Some(issue) = value.get("issue").and_then(Value::as_u64) {
        lines.field(0, "Issue", &format!("#{issue}"));
    }
    if let Some(current) = value
        .get("current")
        .and_then(|current| current.get("name"))
        .and_then(Value::as_str)
    {
        lines.field(0, "Current status", current);
    }
    let allowed = value
        .get("allowed_next")
        .and_then(Value::as_array)
        .map(|states| {
            states
                .iter()
                .filter_map(|state| state.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if allowed.is_empty() {
        lines.field(0, "Next allowed", "(none)");
    } else {
        lines.field(0, "Next allowed", &allowed);
    }
    let missing = value
        .get("allowed_next_missing_on_server")
        .and_then(Value::as_array)
        .map(|states| {
            states
                .iter()
                .filter_map(|state| state.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if !missing.is_empty() {
        lines.field(0, "Missing on server", &missing);
    }
    if value
        .get("advisory")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.field(0, "Advisory", "yes");
    }
    if let Some(caveat) = value.get("caveat").and_then(Value::as_str) {
        lines.field(0, "Caveat", caveat);
    }
    if let Some(recovery) = value.get("recovery").and_then(Value::as_str) {
        lines.field(0, "Recovery", recovery);
    }
    lines.finish()
}

/// The curated `status set` and `status advance` rendering.
pub(crate) fn render_transition(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    lines.heading("Status change");
    lines.blank();
    if let Some(issue) = value.get("issue").and_then(Value::as_u64) {
        lines.field(0, "Issue", &format!("#{issue}"));
    }
    let from = value
        .get("from")
        .and_then(|from| from.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let to = value
        .get("to")
        .and_then(|to| to.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    lines.field(0, "Status", &format!("{from} -> {to}"));
    let changed = value
        .get("changed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    lines.field(
        0,
        "Changed",
        if changed {
            "yes"
        } else {
            "no (already in that status)"
        },
    );
    if value
        .get("advisory")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.field(0, "Advisory", "yes");
    }
    if let Some(caveat) = value.get("caveat").and_then(Value::as_str) {
        lines.field(0, "Caveat", caveat);
    }
    lines.finish()
}

/// The curated `status list` rendering.
pub(crate) fn render_list(value: &Value, context: &OutputContext) -> String {
    let Some(statuses) = value.as_array() else {
        return super::generic::render(value, context);
    };
    let mut lines = Lines::new(context);
    let noun = if statuses.len() == 1 {
        "status"
    } else {
        "statuses"
    };
    lines.heading(&format!("{} {noun}", statuses.len()));
    if statuses.is_empty() {
        lines.blank();
        lines.text(0, "The provider defines no issue status.");
        return lines.finish();
    }
    lines.blank();
    for status in statuses {
        let name = status
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)");
        match status.get("id").and_then(Value::as_u64) {
            Some(id) => lines.item(0, &format!("{id}  {name}")),
            None => lines.item(0, name),
        }
    }
    lines.finish()
}
