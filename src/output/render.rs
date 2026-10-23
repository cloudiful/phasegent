//! The shared rendering toolkit.
//!
//! Every human view renders the same redacted data the JSON document carries.
//! The toolkit owns the rules the views share: terminal control sequences are
//! stripped from untrusted values, long values wrap to the terminal width, and
//! the field labels are emphasized only when the stream may carry color. The
//! views never inspect the data for secrets and never call a provider or the
//! filesystem.

use console::Style;
use serde_json::Value;

use crate::output::context::OutputContext;

/// Render one result value as readable text, using the view selected for the
/// command.
pub fn render_value(value: &Value, context: &OutputContext) -> String {
    crate::output::human::render(context.view, value, context)
}

/// Render one diagnostic (an error or a warning) as readable text.
pub fn render_diagnostic(kind: &str, payload: &Value, color: bool) -> String {
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let operation = payload
        .get("operation")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let label = diagnostic_label(kind, color);
    let mut lines = vec![format!("{label}: {}", sanitize(&message))];
    // The operation is only added when the message does not already name it,
    // so a diagnostic never repeats the same fact twice.
    if let Some(operation) = operation
        && !message.contains(&operation)
    {
        lines.push(format!("  operation: {}", sanitize(&operation)));
    }
    lines.join("\n")
}

/// The diagnostic label, colored only when the stream may carry color.
fn diagnostic_label(kind: &str, color: bool) -> String {
    let style = match kind {
        "warning" => Style::new().yellow().bold(),
        _ => Style::new().red().bold(),
    };
    style.force_styling(color).apply_to(kind).to_string()
}

/// A line buffer that applies the shared wrapping, sanitizing, and label
/// rules. The views build their output through it, so every human view
/// wraps and sanitizes the same way.
pub(crate) struct Lines {
    lines: Vec<String>,
    width: usize,
    color: bool,
}

impl Lines {
    /// A buffer bound to the terminal preferences of the run.
    pub(crate) fn new(context: &OutputContext) -> Self {
        Self {
            lines: Vec::new(),
            width: context.width,
            color: context.color,
        }
    }

    /// The usable width of the buffer.
    #[allow(dead_code)] // read by the views that measure their own columns
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// A section heading.
    ///
    /// A heading is a single-line identity: an embedded line break in untrusted
    /// content (an issue title) never starts a line of its own, and a long
    /// heading wraps to the terminal width instead of overflowing it. Every
    /// wrapped continuation is indented, so a title can never place a forged
    /// field at column zero — not even when the wrap recreates the boundary.
    pub(crate) fn heading(&mut self, title: &str) {
        let text = single_line(title);
        let width = self.width.saturating_sub(CONTINUATION_INDENT).max(1);
        for (index, line) in wrap_line(&text, width).into_iter().enumerate() {
            if index == 0 {
                self.lines.push(line);
            } else {
                self.lines
                    .push(format!("{}{line}", " ".repeat(CONTINUATION_INDENT)));
            }
        }
    }

    /// One structural row: a prune action, a count, or a summary line.
    ///
    /// The row wraps at the terminal width and keeps every character, so a
    /// long path, id, or reason is never truncated; a line break inside the
    /// text never starts a line of its own.
    pub(crate) fn row(&mut self, text: impl AsRef<str>) {
        self.push_single_line(text.as_ref());
    }

    /// A blank separator line.
    pub(crate) fn blank(&mut self) {
        self.lines.push(String::new());
    }

    /// A label with a nested block below it.
    pub(crate) fn group(&mut self, indent: usize, label: &str) {
        self.push_label(indent, "", label);
    }

    /// One `Label: value` line. The value wraps under the value column, and a
    /// multiline value keeps its line structure.
    ///
    /// A label that would leave less than one usable value column is stacked
    /// above its value instead: the label keeps every character and the value
    /// moves to an indented block, so the line never exceeds the width.
    pub(crate) fn field(&mut self, indent: usize, label: &str, value: &str) {
        // The alignment uses the plain label: the styled label carries escape
        // sequences that must not count towards the column width.
        let label = single_line(label);
        let prefix_width = console::measure_text_width(&format!("{label}: "));
        if indent + prefix_width + MIN_VALUE_WIDTH <= self.width {
            let styled = field_label(&label, self.color);
            self.push_wrapped(indent, &format!("{styled}: "), prefix_width, value);
        } else {
            self.push_label(indent, "", &label);
            self.push_wrapped(indent + CONTINUATION_INDENT, "", 0, value);
        }
    }

    /// One list item line.
    pub(crate) fn item(&mut self, indent: usize, text: &str) {
        let available = self.width.saturating_sub(indent + 2).max(1);
        let sanitized = sanitize(text);
        let mut first = true;
        for line in sanitized.split('\n') {
            for wrapped in wrap_line(line, available) {
                if first {
                    self.lines
                        .push(format!("{}- {wrapped}", " ".repeat(indent)));
                    first = false;
                } else {
                    self.lines
                        .push(format!("{}  {wrapped}", " ".repeat(indent)));
                }
            }
        }
    }

    /// One list item that carries a labeled value. The marker sits in front
    /// of the label, and the following fields of the same item align under
    /// it. A label that cannot share the line with a usable value column
    /// stacks, exactly like [`Lines::field`].
    pub(crate) fn item_field(&mut self, indent: usize, label: &str, value: &str) {
        let label = single_line(label);
        let prefix_width = console::measure_text_width(&format!("- {label}: "));
        if indent + prefix_width + MIN_VALUE_WIDTH <= self.width {
            let styled = field_label(&label, self.color);
            self.push_wrapped(indent, &format!("- {styled}: "), prefix_width, value);
        } else {
            self.push_label(indent, "- ", &label);
            self.push_wrapped(indent + CONTINUATION_INDENT, "", 0, value);
        }
    }

    /// One free-form line, wrapped to the width.
    pub(crate) fn text(&mut self, indent: usize, text: &str) {
        let available = self.width.saturating_sub(indent).max(1);
        let sanitized = sanitize(text);
        for line in sanitized.split('\n') {
            for wrapped in wrap_line(line, available) {
                self.lines.push(format!("{}{wrapped}", " ".repeat(indent)));
            }
        }
    }

    /// One preformatted line. The text is sanitized but never wrapped, so a
    /// view can align its own columns.
    pub(crate) fn raw(&mut self, line: impl AsRef<str>) {
        self.lines.push(sanitize(line.as_ref()));
    }

    /// The rendered lines.
    pub(crate) fn finish(self) -> String {
        self.lines.join("\n")
    }

    /// Push one single-line structural text, wrapped to the width.
    fn push_single_line(&mut self, text: &str) {
        for wrapped in wrap_line(&single_line(text), self.width) {
            self.lines.push(wrapped);
        }
    }

    /// Push one `marker{label}:` line.
    ///
    /// The label is emphasized when it fits. A label wider than the remaining
    /// line wraps with an indented continuation, so a long shipped setting
    /// name or a long provider key keeps every character without pushing the
    /// line past the terminal width or reading as a new structural field.
    fn push_label(&mut self, indent: usize, marker: &str, label: &str) {
        let label = single_line(label);
        let plain = format!("{marker}{label}:");
        let pad = " ".repeat(indent);
        if indent + console::measure_text_width(&plain) <= self.width {
            let styled = field_label(&label, self.color);
            self.lines.push(format!("{pad}{marker}{styled}:"));
            return;
        }
        let width = self
            .width
            .saturating_sub(indent + CONTINUATION_INDENT)
            .max(1);
        for (index, line) in wrap_line(&plain, width).into_iter().enumerate() {
            if index == 0 {
                self.lines.push(format!("{pad}{line}"));
            } else {
                self.lines
                    .push(format!("{pad}{}{line}", " ".repeat(CONTINUATION_INDENT)));
            }
        }
    }

    /// Push a wrapped value under a prefix.
    fn push_wrapped(&mut self, indent: usize, prefix: &str, prefix_width: usize, value: &str) {
        let available = self.width.saturating_sub(indent + prefix_width).max(1);
        let sanitized = sanitize(value);
        let mut first = true;
        for line in sanitized.split('\n') {
            for wrapped in wrap_line(line, available) {
                if first {
                    self.lines
                        .push(format!("{}{prefix}{wrapped}", " ".repeat(indent)));
                    first = false;
                } else if wrapped.is_empty() {
                    self.lines.push(String::new());
                } else {
                    self.lines
                        .push(format!("{}{wrapped}", " ".repeat(indent + prefix_width)));
                }
            }
        }
        if first {
            self.lines.push(format!("{}{prefix}", " ".repeat(indent)));
        }
    }
}

/// The narrowest value column an aligned field may use. A label that would
/// leave less than this stacks above its value instead of overflowing.
pub(crate) const MIN_VALUE_WIDTH: usize = 20;

/// The indentation of a wrapped continuation of a heading or a long label, so
/// a continuation never reads as a structural field line.
pub(crate) const CONTINUATION_INDENT: usize = 2;

/// Wrap one line to the available width, breaking on spaces and hard-splitting
/// a token that cannot fit on a line of its own.
pub(crate) fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if width == 0 || console::measure_text_width(line) <= width {
        return vec![line.to_owned()];
    }
    let mut wrapped: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if console::measure_text_width(&current) + 1 + console::measure_text_width(word)
            <= width
        {
            current.push(' ');
            current.push_str(word);
        } else {
            wrapped.push(std::mem::take(&mut current));
            current.push_str(word);
        }
        while console::measure_text_width(&current) > width {
            let head = console::truncate_str(&current, width, "").to_string();
            if head.is_empty() {
                break;
            }
            let rest = current[head.len()..].to_owned();
            wrapped.push(head);
            current = rest;
        }
    }
    if !current.is_empty() {
        wrapped.push(current);
    }
    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
}

/// The label of one field, emphasized when the stream may carry color.
pub(crate) fn field_label(label: &str, color: bool) -> String {
    Style::new()
        .bold()
        .force_styling(color)
        .apply_to(label)
        .to_string()
}

/// Turn a machine key into a readable label.
pub(crate) fn humanize(key: &str) -> String {
    let spaced: String = key
        .chars()
        .map(|character| match character {
            '_' | '-' => ' ',
            other => other,
        })
        .collect();
    let mut characters = spaced.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// Collapse untrusted text onto one structural line.
///
/// Control sequences and control characters are stripped and an embedded line
/// break becomes a single space, so content can never imitate a field or
/// diagnostic line of its own. Real multiline content goes through the
/// multiline APIs ([`Lines::text`], [`Lines::field`]), which indent the
/// continuation lines instead.
pub(crate) fn single_line(text: &str) -> String {
    let sanitized = sanitize(text);
    let mut out = String::with_capacity(sanitized.len());
    let mut pending_space = false;
    for character in sanitized.chars() {
        if character == '\n' {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(character);
    }
    out
}

/// Strip terminal control sequences and control characters from untrusted
/// text while keeping readable multiline content.
pub(crate) fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\u{1b}' => skip_escape(&mut characters),
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            other if other.is_control() => {}
            other => out.push(other),
        }
    }
    out
}

/// Drop the remainder of an escape sequence.
fn skip_escape(characters: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match characters.next() {
        // CSI: parameter and intermediate bytes, then one final byte.
        Some('[') => {
            for character in characters.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&character) {
                    break;
                }
            }
        }
        // OSC: terminated by BEL or by the string terminator.
        Some(']') => {
            while let Some(character) = characters.next() {
                if character == '\u{7}' {
                    break;
                }
                if character == '\u{1b}' {
                    characters.next();
                    break;
                }
            }
        }
        // Any other two-byte escape sequence.
        Some(_) | None => {}
    }
}
