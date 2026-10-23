//! The generic human view: a readable key/value rendering of any result.
//!
//! The generic view is the fallback for every command without a curated
//! view. It never dumps JSON: nested maps and arrays become indented blocks,
//! scalars become labeled lines, and empty collections and nulls become
//! explicit empty states.

use serde_json::Value;

use crate::output::context::OutputContext;
use crate::output::render::{Lines, humanize};

/// Render one value with the generic readable form.
pub(crate) fn render(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    render_node(value, 0, context, &mut lines);
    let rendered = lines.finish();
    if rendered.trim().is_empty() {
        "(empty)".to_owned()
    } else {
        rendered
    }
}

/// Render a node into the output lines.
fn render_node(value: &Value, indent: usize, context: &OutputContext, lines: &mut Lines) {
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                lines.text(indent, "(empty)");
                return;
            }
            for (key, child) in map {
                let label = humanize(key);
                match child {
                    Value::Object(inner) if inner.is_empty() => {
                        lines.field(indent, &label, "(empty)");
                    }
                    Value::Object(_) => {
                        lines.group(indent, &label);
                        render_node(child, indent + 1, context, lines);
                    }
                    Value::Array(items) if items.is_empty() => {
                        lines.field(indent, &label, "(empty)");
                    }
                    Value::Array(_) => {
                        lines.group(indent, &label);
                        render_node(child, indent + 1, context, lines);
                    }
                    Value::Null => {
                        lines.field(indent, &label, "(null)");
                    }
                    Value::String(text) => {
                        lines.field(indent, &label, text);
                    }
                    scalar => {
                        lines.field(indent, &label, &scalar.to_string());
                    }
                }
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                lines.text(indent, "(empty)");
                return;
            }
            for item in items {
                match item {
                    Value::Object(map) if map.is_empty() => {
                        lines.item(indent, "(empty)");
                    }
                    Value::Object(map) => render_list_object(lines, indent, context, map),
                    Value::Array(_) => {
                        // A list of lists keeps every element's own marker one
                        // level deeper, so the block stays width-aware instead
                        // of re-prefixing pre-wrapped lines.
                        render_node(item, indent + 1, context, lines);
                    }
                    Value::Null => {
                        lines.item(indent, "(null)");
                    }
                    Value::String(text) => {
                        lines.item(indent, text);
                    }
                    scalar => {
                        lines.item(indent, &scalar.to_string());
                    }
                }
            }
        }
        Value::String(text) => {
            lines.text(indent, text);
        }
        Value::Null => {
            lines.text(indent, "(null)");
        }
        scalar => {
            lines.text(indent, &scalar.to_string());
        }
    }
}

/// Render one list item that is an object.
///
/// The first emitted field carries the list marker and the following fields
/// align under it. A nested object or array becomes a keyed block one level
/// deeper instead of a serialized JSON value, so a list item never dumps the
/// machine shape back at the reader.
fn render_list_object(
    lines: &mut Lines,
    indent: usize,
    context: &OutputContext,
    map: &serde_json::Map<String, Value>,
) {
    let mut marked = false;
    for (key, child) in map {
        let label = humanize(key);
        match child {
            Value::Object(inner) if inner.is_empty() => {
                emit_list_scalar(lines, indent, &mut marked, &label, "(empty)");
            }
            Value::Array(items) if items.is_empty() => {
                emit_list_scalar(lines, indent, &mut marked, &label, "(empty)");
            }
            Value::Null => emit_list_scalar(lines, indent, &mut marked, &label, "(null)"),
            Value::String(text) => emit_list_scalar(lines, indent, &mut marked, &label, text),
            Value::Bool(_) | Value::Number(_) => {
                emit_list_scalar(lines, indent, &mut marked, &label, &child.to_string());
            }
            Value::Object(_) | Value::Array(_) => {
                // The item marker stays on the collection label; the children
                // sit one level deeper than the sibling fields.
                if marked {
                    lines.group(indent + 1, &label);
                } else {
                    lines.item(indent, &format!("{label}:"));
                    marked = true;
                }
                render_node(child, indent + 2, context, lines);
            }
        }
    }
}

/// Emit one scalar field of a list item, on the marker line first.
fn emit_list_scalar(lines: &mut Lines, indent: usize, marked: &mut bool, label: &str, text: &str) {
    if *marked {
        lines.field(indent + 1, label, text);
    } else {
        lines.item_field(indent, label, text);
        *marked = true;
    }
}
