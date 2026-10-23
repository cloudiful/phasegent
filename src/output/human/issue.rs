//! The curated issue views: the issue detail, the search result, and the
//! concise write result.

use serde_json::Value;

use crate::output::context::OutputContext;
use crate::output::render::{Lines, single_line};

/// The curated `issue get` rendering.
pub(crate) fn render_detail(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    lines.heading(&issue_heading(value));
    if let Some(state) = value.get("state").and_then(Value::as_str) {
        lines.field(0, "State", state);
    }
    if let Some(project) = project_label(value) {
        lines.field(0, "Project", &project);
    }
    if let Some(url) = value.get("html_url").and_then(Value::as_str) {
        lines.field(0, "URL", url);
    }
    lines.blank();
    lines.group(0, "Body");
    let body = value
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if body.trim().is_empty() {
        lines.text(1, "(empty)");
    } else {
        lines.text(1, body);
    }
    lines.finish()
}

/// The curated `issue search` rendering.
pub(crate) fn render_search(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    let items = value.get("items").and_then(Value::as_array);
    let Some(items) = items else {
        return super::generic::render(value, context);
    };
    lines.heading(&search_summary(value, items.len()));
    if items.is_empty() {
        lines.blank();
        lines.text(0, "No issues matched the search.");
        return lines.finish();
    }
    lines.blank();
    // The state column keeps a fixed width so the rows stay readable.
    let state_width = items
        .iter()
        .filter_map(|item| item.get("state").and_then(Value::as_str))
        .map(console::measure_text_width)
        .max()
        .unwrap_or(0);
    for item in items {
        let number = item
            .get("number")
            .and_then(Value::as_u64)
            .or_else(|| item.get("id").and_then(Value::as_u64))
            .map(|number| format!("#{number}"))
            .unwrap_or_else(|| "-".to_owned());
        let state = item.get("state").and_then(Value::as_str).unwrap_or("-");
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(no title)");
        let padding = " ".repeat(state_width.saturating_sub(console::measure_text_width(state)));
        // The row is one structural line: a line break in an untrusted title
        // never starts a line of its own.
        lines.item(
            0,
            &format!("{number}  {state}{padding}  {}", single_line(title)),
        );
    }
    lines.finish()
}

/// The curated `issue create`, `issue update`, and `issue close` rendering.
/// The written body is not echoed back; the result states what changed.
pub(crate) fn render_write(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    lines.heading(&issue_heading(value));
    if let Some(state) = value.get("state").and_then(Value::as_str) {
        lines.field(0, "State", state);
    }
    if let Some(project) = project_label(value) {
        lines.field(0, "Project", &project);
    }
    if let Some(url) = value.get("html_url").and_then(Value::as_str) {
        lines.field(0, "URL", url);
    }
    lines.finish()
}

/// The heading of one issue: its number and its single-line title. The
/// heading itself keeps untrusted content on one line, so a title can never
/// imitate a field line.
fn issue_heading(value: &Value) -> String {
    let number = value
        .get("number")
        .and_then(Value::as_u64)
        .or_else(|| value.get("id").and_then(Value::as_u64));
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("(no title)");
    match number {
        Some(number) => format!("#{number} {title}"),
        None => title.to_owned(),
    }
}

/// The readable owning project of one issue.
fn project_label(value: &Value) -> Option<String> {
    let project = value.get("project")?;
    let name = project.get("name").and_then(Value::as_str)?;
    let identifier = project.get("identifier").and_then(Value::as_str);
    match identifier {
        Some(identifier) if identifier != name => Some(format!("{name} ({identifier})")),
        _ => Some(name.to_owned()),
    }
}

/// The summary line of a search result.
fn search_summary(value: &Value, shown: usize) -> String {
    let total = value
        .get("total_count")
        .and_then(Value::as_u64)
        .unwrap_or(shown as u64);
    let page = value.get("page").and_then(Value::as_u64).unwrap_or(1);
    let limit = value.get("limit").and_then(Value::as_u64).unwrap_or(0);
    let noun = if total == 1 { "result" } else { "results" };
    let mut summary = format!("{total} {noun} (page {page}, limit {limit})");
    if value
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        summary.push_str(", more available");
    }
    if let Some(source) = value.get("data_source").and_then(Value::as_str) {
        summary.push_str(&format!(", source {source}"));
    }
    if value.get("stale").and_then(Value::as_bool).unwrap_or(false) {
        summary.push_str(", stale");
    }
    summary
}
