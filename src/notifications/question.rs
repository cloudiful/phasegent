//! Structured question notification input and rendering (issue #1026 P2).
//!
//! `phasegent notify send --stdin` reads one UTF-8 JSON document describing a
//! pending OpenCode question and renders a blocked notification with a bounded
//! multiline plain alternative plus a fixed, inline-styled HTML alternative.
//! The HTML carries no scripts, forms, remote assets, tracking, or untrusted
//! URLs, and every interpolated scalar is escaped. Nothing on the rendered
//! notification is persisted: only the bounded plain title/body reach storage.
//!
//! Input is bounded twice. The reader accepts at most
//! [`QUESTION_INPUT_LIMIT_BYTES`] bytes, then normalization enforces per-field
//! caps, at most [`MAX_QUESTIONS`] questions and [`MAX_CHOICES`] choices each,
//! and one shared text budget. Over-limit content is clipped with a visible
//! ellipsis or an explicit "omitted" line, never a cut HTML tag.

use std::io::Read;

use serde::Deserialize;

use crate::notifications::envelope::NotificationIntent;

/// Maximum accepted stdin bytes: 65536 payload bytes plus one sentinel byte
/// that detects overflow.
pub const QUESTION_INPUT_LIMIT_BYTES: usize = 65536;
/// At most this many questions are rendered; extra entries are omitted.
pub const MAX_QUESTIONS: usize = 8;
/// At most this many choices per question; extra entries are omitted.
pub const MAX_CHOICES: usize = 12;
/// Shared text budget across every retained field, in Unicode scalars.
pub const TOTAL_TEXT_BUDGET: usize = 8000;

const SESSION_ID_CAP: usize = 128;
const SESSION_TITLE_CAP: usize = 200;
const PROJECT_CAP: usize = 240;
const HEADER_CAP: usize = 120;
const QUESTION_TEXT_CAP: usize = 1500;
const LABEL_CAP: usize = 160;
const DESCRIPTION_CAP: usize = 400;
const SUBJECT_LIMIT: usize = 140;

/// Fixed rejection categories. No serde message, raw payload, or secret is
/// ever surfaced; the CLI maps a variant to its fixed message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionError {
    /// stdin read failed.
    ReadFailed,
    /// More than the byte limit was supplied.
    TooLarge,
    /// Not valid JSON, an unknown field, or a wrong-typed value.
    Malformed,
    /// `event` was not the supported `blocked` literal.
    UnsupportedEvent,
    /// `session_id` was missing or empty.
    MissingSessionId,
    /// No questions were supplied.
    MissingQuestions,
    /// A question had empty text.
    MissingQuestionText,
}

impl QuestionError {
    /// Fixed, payload-free category message.
    pub const fn category(self) -> &'static str {
        match self {
            Self::ReadFailed => "could not read notification input",
            Self::TooLarge => "notification input is too large",
            Self::Malformed => "notification input is not valid structured JSON",
            Self::UnsupportedEvent => "notification event is not supported",
            Self::MissingSessionId => "notification session id is required",
            Self::MissingQuestions => "notification questions are required",
            Self::MissingQuestionText => "notification question text is required",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInput {
    event: String,
    question: RawQuestion,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQuestion {
    session_id: String,
    #[serde(default)]
    session_title: Option<String>,
    #[serde(default)]
    project: Option<String>,
    questions: Vec<RawItem>,
    /// Questions the producer already dropped. Combined with the count this
    /// renderer drops itself, so the recipient always sees the real total.
    #[serde(default)]
    omitted_questions: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    text: String,
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    multiple: Option<bool>,
    #[serde(default)]
    options: Vec<RawOption>,
    /// Choices the producer already dropped for this question.
    #[serde(default)]
    omitted_options: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOption {
    label: String,
    #[serde(default)]
    description: Option<String>,
}

/// Normalized, bounded question content ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionNotification {
    pub session_id: String,
    pub session_title: String,
    pub project: String,
    pub questions: Vec<NormalizedQuestion>,
    pub omitted_questions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedQuestion {
    pub header: String,
    pub text: String,
    pub multiple: bool,
    pub options: Vec<NormalizedOption>,
    pub omitted_options: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedOption {
    pub label: String,
    pub description: String,
}

/// Read at most [`QUESTION_INPUT_LIMIT_BYTES`] bytes from `reader`, failing
/// with [`QuestionError::TooLarge`] when one extra sentinel byte is present.
/// Kept separate from parsing so the CLI can check authority before reading.
pub fn read_question_input<R: Read>(reader: R) -> Result<Vec<u8>, QuestionError> {
    let mut buffer = Vec::new();
    let mut limited = reader.take((QUESTION_INPUT_LIMIT_BYTES as u64) + 1);
    limited
        .read_to_end(&mut buffer)
        .map_err(|_| QuestionError::ReadFailed)?;
    if buffer.len() > QUESTION_INPUT_LIMIT_BYTES {
        return Err(QuestionError::TooLarge);
    }
    Ok(buffer)
}

/// Parse and normalize a UTF-8 JSON question document. Unknown fields,
/// wrong types, and malformed JSON all collapse into
/// [`QuestionError::Malformed`] so no serde text leaks.
pub fn parse_question_input(bytes: &[u8]) -> Result<QuestionNotification, QuestionError> {
    let raw: RawInput = serde_json::from_slice(bytes).map_err(|_| QuestionError::Malformed)?;
    if raw.event != "blocked" {
        return Err(QuestionError::UnsupportedEvent);
    }
    let session_id = clean_text(&raw.question.session_id);
    if session_id.is_empty() {
        return Err(QuestionError::MissingSessionId);
    }
    if raw.question.questions.is_empty() {
        return Err(QuestionError::MissingQuestions);
    }
    for item in &raw.question.questions {
        if clean_text(&item.text).is_empty() {
            return Err(QuestionError::MissingQuestionText);
        }
    }
    Ok(normalize(session_id, raw.question))
}

fn normalize(session_id: String, raw: RawQuestion) -> QuestionNotification {
    let mut budget = Budget::new(TOTAL_TEXT_BUDGET);
    let mut session_id = budget.take(&session_id, SESSION_ID_CAP);
    if session_id.is_empty() {
        // session_id is validated nonempty before normalization; the budget is
        // far larger than its cap, so this only guards a degenerate budget.
        session_id = String::from("unknown");
    }
    let session_title = budget.take(
        raw.session_title.as_deref().unwrap_or(""),
        SESSION_TITLE_CAP,
    );
    let project = budget.take(raw.project.as_deref().unwrap_or(""), PROJECT_CAP);

    let mut questions = Vec::new();
    let mut omitted_questions = bounded_omission(raw.omitted_questions);
    for item in raw.questions.iter().take(MAX_QUESTIONS) {
        // The question text is mandatory, so it is allocated before the
        // optional header and choices: neither of them can starve it.
        let text = budget.take_question_text(&item.text, QUESTION_TEXT_CAP);
        if text.is_empty() {
            // No real question text fits, so the whole question is omitted and
            // counted instead of being rendered empty.
            omitted_questions += 1;
            continue;
        }
        let header = budget.take(item.header.as_deref().unwrap_or(""), HEADER_CAP);
        let mut options = Vec::new();
        let mut omitted_options = bounded_omission(item.omitted_options);
        for option in item.options.iter().take(MAX_CHOICES) {
            if budget.remaining == 0 {
                omitted_options += 1;
                continue;
            }
            let label = budget.take(&option.label, LABEL_CAP);
            let description =
                budget.take(option.description.as_deref().unwrap_or(""), DESCRIPTION_CAP);
            if label.is_empty() && description.is_empty() {
                omitted_options += 1;
                continue;
            }
            options.push(NormalizedOption { label, description });
        }
        omitted_options += item.options.len().saturating_sub(MAX_CHOICES);
        questions.push(NormalizedQuestion {
            header,
            text,
            multiple: item.multiple.unwrap_or(false),
            options,
            omitted_options,
        });
    }
    omitted_questions += raw.questions.len().saturating_sub(MAX_QUESTIONS);

    QuestionNotification {
        session_id,
        session_title,
        project,
        questions,
        omitted_questions,
    }
}

/// Upper bound for a producer-supplied omission count. The count is display
/// metadata only, so it is clamped: a hostile value must not overflow the
/// rendered count.
const MAX_OMISSION_COUNT: u64 = 1_000_000;

/// Clamp an optional producer-supplied omission count.
fn bounded_omission(value: Option<u64>) -> usize {
    value.unwrap_or(0).min(MAX_OMISSION_COUNT) as usize
}

impl QuestionNotification {
    /// Render the bounded subject, multiline plain body, and HTML alternative.
    pub fn to_intent(&self) -> NotificationIntent {
        let subject_name = if self.session_title.is_empty() {
            self.session_id.as_str()
        } else {
            self.session_title.as_str()
        };
        let subject = clip(&format!("OpenCode question: {subject_name}"), SUBJECT_LIMIT);
        NotificationIntent::new_question(subject, self.render_text(), self.render_html())
    }

    fn render_text(&self) -> String {
        let mut out = String::from("A question is waiting in OpenCode.\n");
        if !self.project.is_empty() {
            out.push_str(&format!("Project: {}\n", self.project));
        }
        if !self.session_title.is_empty() {
            out.push_str(&format!("Conversation: {}\n", self.session_title));
        }
        for (index, question) in self.questions.iter().enumerate() {
            out.push_str(&format!("\n{}.", index + 1));
            if !question.header.is_empty() {
                out.push_str(&format!(" {}", question.header));
            }
            out.push('\n');
            out.push_str(&format!("   {}\n", question.text));
            if question.multiple {
                out.push_str("   (Select all that apply)\n");
            }
            for option in &question.options {
                if option.description.is_empty() {
                    out.push_str(&format!("   - {}\n", option.label));
                } else {
                    out.push_str(&format!("   - {}: {}\n", option.label, option.description));
                }
            }
            if question.omitted_options > 0 {
                out.push_str(&format!(
                    "   ({} more choices omitted)\n",
                    question.omitted_options
                ));
            }
        }
        if self.omitted_questions > 0 {
            out.push_str(&format!(
                "\n({} more questions omitted)\n",
                self.omitted_questions
            ));
        }
        out.push_str("\nAnswer in OpenCode. Replies to this email are not read.\n");
        out.push_str(&format!("Session: {}", self.session_id));
        out
    }

    fn render_html(&self) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body style=\"margin:0;padding:0;background:#f4f5f7;\">\
<div style=\"font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#1f2328;font-size:15px;line-height:1.5;max-width:680px;margin:0 auto;padding:24px;\">",
        );
        out.push_str(
            "<p style=\"margin:0 0 4px;font-size:13px;letter-spacing:.02em;text-transform:uppercase;color:#57606a;\">OpenCode question</p>",
        );
        let title = if self.session_title.is_empty() {
            self.session_id.as_str()
        } else {
            self.session_title.as_str()
        };
        out.push_str(&format!(
            "<h1 style=\"margin:0 0 4px;font-size:20px;font-weight:600;\">{}</h1>",
            escape_html(&clip(title, SESSION_TITLE_CAP))
        ));
        if !self.project.is_empty() {
            out.push_str(&format!(
                "<p style=\"margin:0 0 16px;color:#57606a;\">Project: {}</p>",
                escape_html(&self.project)
            ));
        }
        out.push_str("<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" style=\"width:100%;border-collapse:collapse;\">");
        for (index, question) in self.questions.iter().enumerate() {
            out.push_str(
                "<tr><td style=\"padding:12px 14px;border:1px solid #d0d7de;border-radius:8px;background:#ffffff;\">",
            );
            out.push_str(&format!(
                "<p style=\"margin:0 0 6px;font-weight:600;\">{}",
                index + 1
            ));
            if !question.header.is_empty() {
                out.push_str(&format!(" &middot; {}", escape_html(&question.header)));
            }
            out.push_str("</p>");
            out.push_str(&format!(
                "<p style=\"margin:0 0 8px;white-space:pre-wrap;\">{}</p>",
                escape_html(&question.text).replace('\n', "<br>")
            ));
            if question.multiple {
                out.push_str(
                    "<p style=\"margin:0 0 8px;color:#57606a;font-style:italic;\">Select all that apply</p>",
                );
            }
            if !question.options.is_empty() {
                out.push_str("<ul style=\"margin:0;padding-left:20px;\">");
                for option in &question.options {
                    let label = escape_html(&option.label);
                    if option.description.is_empty() {
                        out.push_str(&format!("<li>{label}</li>"));
                    } else {
                        out.push_str(&format!(
                            "<li><strong>{label}</strong> &mdash; {}</li>",
                            escape_html(&option.description)
                        ));
                    }
                }
                if question.omitted_options > 0 {
                    out.push_str(&format!(
                        "<li style=\"color:#57606a;\"><em>{} more choices omitted</em></li>",
                        question.omitted_options
                    ));
                }
                out.push_str("</ul>");
            } else if question.omitted_options > 0 {
                // Every choice was dropped: the omission still has to be visible.
                out.push_str(&format!(
                    "<p style=\"margin:0;color:#57606a;\"><em>{} more choices omitted</em></p>",
                    question.omitted_options
                ));
            }
            out.push_str("</td></tr>");
        }
        out.push_str("</table>");
        if self.omitted_questions > 0 {
            out.push_str(&format!(
                "<p style=\"margin:12px 0 0;color:#57606a;\"><em>{} more questions omitted</em></p>",
                self.omitted_questions
            ));
        }
        out.push_str(
            "<p style=\"margin:16px 0 0;color:#57606a;\">Answer in OpenCode. Replies to this email are not read.</p>",
        );
        out.push_str(&format!(
            "<p style=\"margin:4px 0 0;color:#8c959f;font-size:12px;\">Session {}</p>",
            escape_html(&self.session_id)
        ));
        out.push_str("</div></body></html>");
        out
    }
}

struct Budget {
    remaining: usize,
}

impl Budget {
    fn new(total: usize) -> Self {
        Self { remaining: total }
    }

    /// Clean `raw`, cap it to `cap` scalars and the remaining shared budget,
    /// and charge what it emits. Clipping appends a single `…` inside the cap.
    fn take(&mut self, raw: &str, cap: usize) -> String {
        let cleaned = clean_text(raw);
        if cleaned.is_empty() {
            return String::new();
        }
        let available = cap.min(self.remaining);
        if available == 0 {
            return String::new();
        }
        let count = cleaned.chars().count();
        let emitted = if count <= available {
            cleaned
        } else {
            let keep = available.saturating_sub(1);
            let mut clipped: String = cleaned.chars().take(keep).collect();
            clipped.push('…');
            clipped
        };
        self.remaining = self.remaining.saturating_sub(emitted.chars().count());
        emitted
    }

    /// Take the mandatory question text. Only real source content counts: a
    /// clip that would leave nothing but the ellipsis carries no question, so
    /// nothing is taken and the caller omits the question instead.
    fn take_question_text(&mut self, raw: &str, cap: usize) -> String {
        let cleaned = clean_text(raw);
        if cleaned.is_empty() {
            return String::new();
        }
        let available = cap.min(self.remaining);
        if available < 2 {
            return String::new();
        }
        let count = cleaned.chars().count();
        let emitted = if count <= available {
            cleaned
        } else {
            let keep = available - 1;
            let mut clipped: String = cleaned.chars().take(keep).collect();
            clipped.push('…');
            clipped
        };
        self.remaining = self.remaining.saturating_sub(emitted.chars().count());
        emitted
    }
}

/// Normalize whitespace-significant text: unify newlines, drop control chars
/// (other than newline), and trim the edges.
fn clean_text(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let filtered: String = normalized
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    filtered.trim().to_owned()
}

fn clip(raw: &str, limit: usize) -> String {
    if raw.chars().count() <= limit {
        return raw.to_owned();
    }
    let keep = limit.saturating_sub(1);
    let mut clipped: String = raw.chars().take(keep).collect();
    clipped.push('…');
    clipped
}

fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> QuestionNotification {
        parse_question_input(json.as_bytes()).expect("valid question input")
    }

    fn minimal() -> &'static str {
        r#"{"event":"blocked","question":{"session_id":"ses_1","questions":[{"text":"Which option?"}]}}"#
    }

    /// Total text content of a normalized document, in Unicode scalars.
    fn content_scalars(input: &QuestionNotification) -> usize {
        let scalars = |value: &str| value.chars().count();
        input
            .questions
            .iter()
            .map(|question| {
                scalars(&question.header)
                    + scalars(&question.text)
                    + question
                        .options
                        .iter()
                        .map(|option| scalars(&option.label) + scalars(&option.description))
                        .sum::<usize>()
            })
            .sum::<usize>()
            + scalars(&input.session_id)
            + scalars(&input.session_title)
            + scalars(&input.project)
    }

    /// The exact reported boundary shape: a full 120-scalar header, a full
    /// 1500-scalar question text, and twelve choices with 160-scalar labels and
    /// 400-scalar descriptions, except a 1-scalar last description.
    fn heavy_question(header: &str) -> String {
        let options: Vec<String> = (0..12)
            .map(|option| {
                let description = if option == 11 {
                    String::from("d")
                } else {
                    "d".repeat(400)
                };
                format!(
                    r#"{{"label":"{}","description":"{}"}}"#,
                    "l".repeat(160),
                    description
                )
            })
            .collect();
        format!(
            r#"{{"header":"{}","text":"{}","options":[{}]}}"#,
            header,
            "q".repeat(1500),
            options.join(",")
        )
    }

    #[test]
    fn a_header_never_starves_the_mandatory_question_text() {
        // The reported example: session `ses_1` and project `repo`, so the first
        // question leaves exactly 50 scalars before question two.
        let json = format!(
            r#"{{"event":"blocked","question":{{"session_id":"ses_1","project":"repo","questions":[{},{{"header":"{}","text":"Actual second question?"}}]}}}}"#,
            heavy_question(&"h".repeat(120)),
            "H".repeat(120)
        );
        let input = parse(&json);
        assert_eq!(
            content_scalars(&input),
            TOTAL_TEXT_BUDGET,
            "the fixture must exhaust the shared budget exactly"
        );
        assert_eq!(input.omitted_questions, 0);
        assert_eq!(input.questions.len(), 2);
        let last = input.questions.last().expect("a retained question");
        // The real second question survives; only its optional header is clipped.
        assert_eq!(last.text, "Actual second question?");
        assert_eq!(last.header, format!("{}…", "H".repeat(26)));
        assert_eq!(last.header.chars().count(), 27);
        // No synthetic stand-in ever replaces a real question.
        assert!(
            input
                .questions
                .iter()
                .all(|question| question.text != "Question")
        );
    }

    #[test]
    fn a_question_that_cannot_retain_real_text_is_omitted_and_counted() {
        let questions: Vec<String> = (0..3).map(|_| heavy_question(&"h".repeat(120))).collect();
        let json = format!(
            r#"{{"event":"blocked","question":{{"session_id":"ses_1","project":"repo","questions":[{}]}}}}"#,
            questions.join(",")
        );
        let input = parse(&json);
        assert!(
            content_scalars(&input) <= TOTAL_TEXT_BUDGET,
            "content exceeds the shared budget"
        );
        assert!(input.questions.len() < 3);
        assert!(input.omitted_questions > 0);
        assert!(
            input
                .questions
                .iter()
                .all(|question| !question.text.is_empty() && question.text != "…")
        );
        let body = input.to_intent().body;
        assert!(body.contains("more questions omitted"));
    }

    #[test]
    fn renders_subject_project_questions_and_choices() {
        let input = parse(
            r#"{"event":"blocked","question":{"session_id":"ses_1","session_title":"Fix the parser","project":"phasegent","questions":[{"header":"Choice","text":"Which option?","multiple":true,"options":[{"label":"A","description":"First option"},{"label":"B"}]}]}}"#,
        );
        let intent = input.to_intent();
        assert_eq!(intent.title, "OpenCode question: Fix the parser");
        assert_eq!(intent.event.as_str(), "blocked");
        assert!(intent.body.contains("Project: phasegent"));
        assert!(intent.body.contains("Conversation: Fix the parser"));
        assert!(intent.body.contains("Which option?"));
        assert!(intent.body.contains("Select all that apply"));
        assert!(intent.body.contains("- A: First option"));
        assert!(intent.body.contains("- B"));
        assert!(intent.body.contains("Session: ses_1"));
        // Rich question bodies do not append the legacy event line.
        assert!(!intent.body.contains("event=blocked"));
        let html = intent.html.expect("html alternative");
        assert!(html.contains("Fix the parser"));
        assert!(html.contains("phasegent"));
        assert!(html.contains("First option"));
        assert!(!html.contains("<script"));
    }

    #[test]
    fn title_falls_back_to_session_id() {
        let intent = parse(minimal()).to_intent();
        assert_eq!(intent.title, "OpenCode question: ses_1");
    }

    #[test]
    fn envelope_carries_html_body_for_rich_notifications() {
        let envelope = parse(minimal()).to_intent().to_message_envelope();
        assert!(envelope.html_body.is_some());
        assert!(envelope.title.is_some());
        // event stays metadata, not visible body text.
        assert!(!envelope.body.contains("event="));
        assert_eq!(
            envelope.metadata.get("event").and_then(|v| v.as_str()),
            Some("blocked")
        );
    }

    #[test]
    fn escapes_malicious_html_and_attributes() {
        let input = parse(
            r#"{"event":"blocked","question":{"session_id":"ses_1","session_title":"<img src=x onerror=alert(1)>","questions":[{"text":"<script>alert(2)</script> & \"q\" 'x'","options":[{"label":"<a href=\"http://evil.test\">click</a>"}]}]}}"#,
        );
        let html = input.to_intent().html.expect("html");
        assert!(html.contains("&lt;script&gt;alert(2)&lt;/script&gt;"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("&quot;q&quot;"));
        assert!(html.contains("&#39;x&#39;"));
        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(html.contains("&lt;a href=&quot;http://evil.test&quot;&gt;click&lt;/a&gt;"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img "));
        assert!(!html.contains("<a "));
    }

    #[test]
    fn free_text_question_has_no_options() {
        let input = parse(
            r#"{"event":"blocked","question":{"session_id":"ses_1","questions":[{"text":"Describe the bug"}]}}"#,
        );
        assert!(input.questions[0].options.is_empty());
        let body = input.to_intent().body;
        assert!(body.contains("Describe the bug"));
        assert!(!body.contains("- "));
        assert!(!body.contains("Select all that apply"));
    }

    #[test]
    fn clips_to_question_and_choice_caps_with_visible_omission() {
        let questions: Vec<String> = (0..10)
            .map(|index| format!(r#"{{"text":"question {index}"}}"#))
            .collect();
        let options: Vec<String> = (0..15)
            .map(|index| format!(r#"{{"label":"option {index}"}}"#))
            .collect();
        let json = format!(
            r#"{{"event":"blocked","question":{{"session_id":"ses_1","questions":[{{"text":"pick","options":[{}]}},{}]}}}}"#,
            options.join(","),
            questions.join(",")
        );
        let input = parse(&json);
        assert_eq!(input.questions.len(), MAX_QUESTIONS);
        assert_eq!(input.omitted_questions, 11 - MAX_QUESTIONS);
        assert_eq!(input.questions[0].options.len(), MAX_CHOICES);
        assert_eq!(input.questions[0].omitted_options, 15 - MAX_CHOICES);
        let body = input.to_intent().body;
        assert!(body.contains("more choices omitted"));
        assert!(body.contains("more questions omitted"));
    }

    #[test]
    fn combines_supplied_and_own_omission_counts() {
        let input = parse(
            r#"{"event":"blocked","question":{"session_id":"ses_1","omitted_questions":3,"questions":[{"text":"pick","options":[{"label":"A"}],"omitted_options":2}]}}"#,
        );
        assert_eq!(input.omitted_questions, 3);
        assert_eq!(input.questions[0].omitted_options, 2);
        let intent = input.to_intent();
        assert!(intent.body.contains("3 more questions omitted"));
        assert!(intent.body.contains("2 more choices omitted"));
        let html = intent.html.expect("html");
        assert!(html.contains("3 more questions omitted"));
        assert!(html.contains("2 more choices omitted"));
    }

    #[test]
    fn shows_choice_omission_when_every_choice_was_dropped() {
        let input = parse(
            r#"{"event":"blocked","question":{"session_id":"ses_1","questions":[{"text":"pick","omitted_options":4}]}}"#,
        );
        assert!(input.questions[0].options.is_empty());
        let intent = input.to_intent();
        assert!(intent.body.contains("4 more choices omitted"));
        let html = intent.html.expect("html");
        assert!(html.contains("4 more choices omitted"));
    }

    #[test]
    fn clamps_supplied_omission_counts() {
        let input = parse(
            r#"{"event":"blocked","question":{"session_id":"ses_1","omitted_questions":18446744073709551615,"questions":[{"text":"pick","omitted_options":18446744073709551615}]}}"#,
        );
        assert_eq!(input.omitted_questions, MAX_OMISSION_COUNT as usize);
        assert_eq!(
            input.questions[0].omitted_options,
            MAX_OMISSION_COUNT as usize
        );
    }

    #[test]
    fn rejects_invalid_omission_counts() {
        for json in [
            r#"{"event":"blocked","question":{"session_id":"ses_1","omitted_questions":-1,"questions":[{"text":"q"}]}}"#,
            r#"{"event":"blocked","question":{"session_id":"ses_1","omitted_questions":1.5,"questions":[{"text":"q"}]}}"#,
            r#"{"event":"blocked","question":{"session_id":"ses_1","questions":[{"text":"q","omitted_options":"2"}]}}"#,
            r#"{"event":"blocked","question":{"session_id":"ses_1","questions":[{"text":"q","omitted_options":-3}]}}"#,
        ] {
            assert_eq!(
                parse_question_input(json.as_bytes()).unwrap_err(),
                QuestionError::Malformed,
                "input should be malformed: {json}"
            );
        }
    }

    #[test]
    fn bounds_per_field_and_total_budget() {
        let long = "x".repeat(5000);
        let json = format!(
            r#"{{"event":"blocked","question":{{"session_id":"ses_1","session_title":"{}","questions":[{{"text":"{}"}}]}}}}"#,
            long, long
        );
        let input = parse(&json);
        assert!(input.session_title.chars().count() <= SESSION_TITLE_CAP);
        assert!(input.questions[0].text.chars().count() <= QUESTION_TEXT_CAP);
        let consumed: usize = input
            .questions
            .iter()
            .map(|q| {
                q.header.chars().count()
                    + q.text.chars().count()
                    + q.options
                        .iter()
                        .map(|o| o.label.chars().count() + o.description.chars().count())
                        .sum::<usize>()
            })
            .sum::<usize>()
            + input.session_id.chars().count()
            + input.session_title.chars().count()
            + input.project.chars().count();
        assert!(consumed <= TOTAL_TEXT_BUDGET);
    }

    #[test]
    fn preserves_unicode_and_multiline_text() {
        let input = parse(
            "{\"event\":\"blocked\",\"question\":{\"session_id\":\"ses_1\",\"questions\":[{\"text\":\"第一行\\n第二行 🎯\"}]}}",
        );
        let intent = input.to_intent();
        assert!(intent.body.contains("第一行\n第二行 🎯"));
        let html = intent.html.expect("html");
        assert!(html.contains("第一行<br>第二行 🎯"));
        // The part declares its encoding so a client renders the text as UTF-8.
        assert!(html.contains("<meta charset=\"utf-8\">"));
    }

    #[test]
    fn empty_optional_strings_are_omitted() {
        let input = parse(
            r#"{"event":"blocked","question":{"session_id":"ses_1","session_title":"","project":"","questions":[{"text":"q","header":"","options":[{"label":"","description":""}]}]}}"#,
        );
        assert!(input.session_title.is_empty());
        assert!(input.project.is_empty());
        assert!(input.questions[0].header.is_empty());
        assert!(input.questions[0].options.is_empty());
        let body = input.to_intent().body;
        assert!(!body.contains("Project:"));
        assert!(!body.contains("Conversation:"));
    }

    #[test]
    fn rejects_unsupported_event() {
        let json = minimal().replace("\"blocked\"", "\"completion\"");
        assert_eq!(
            parse_question_input(json.as_bytes()).unwrap_err(),
            QuestionError::UnsupportedEvent
        );
    }

    #[test]
    fn rejects_unknown_fields_types_and_malformed_json() {
        for json in [
            r#"{"event":"blocked","question":{"session_id":"ses_1","questions":[{"text":"q"}],"extra":1}}"#,
            r#"{"event":"blocked","question":{"session_id":"ses_1","questions":[{"text":5}]}}"#,
            r#"{"event":"blocked","question":{"session_id":"ses_1","questions":"nope"}}"#,
            r#"not json at all"#,
            r#"{"event":"blocked"}"#,
        ] {
            assert_eq!(
                parse_question_input(json.as_bytes()).unwrap_err(),
                QuestionError::Malformed,
                "input should be malformed: {json}"
            );
        }
    }

    #[test]
    fn rejects_missing_required_content() {
        assert_eq!(
            parse_question_input(
                br#"{"event":"blocked","question":{"session_id":"","questions":[{"text":"q"}]}}"#
            )
            .unwrap_err(),
            QuestionError::MissingSessionId
        );
        assert_eq!(
            parse_question_input(
                br#"{"event":"blocked","question":{"session_id":"ses_1","questions":[]}}"#
            )
            .unwrap_err(),
            QuestionError::MissingQuestions
        );
        assert_eq!(
            parse_question_input(
                br#"{"event":"blocked","question":{"session_id":"ses_1","questions":[{"text":""}]}}"#
            )
            .unwrap_err(),
            QuestionError::MissingQuestionText
        );
    }

    #[test]
    fn reader_rejects_oversize_and_missing_input() {
        let ok = vec![b'a'; QUESTION_INPUT_LIMIT_BYTES];
        assert!(read_question_input(ok.as_slice()).is_ok());
        let too_large = vec![b'a'; QUESTION_INPUT_LIMIT_BYTES + 1];
        assert_eq!(
            read_question_input(too_large.as_slice()).unwrap_err(),
            QuestionError::TooLarge
        );
        assert_eq!(
            read_question_input(std::io::empty()).unwrap(),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn error_categories_do_not_embed_payload() {
        for error in [
            QuestionError::ReadFailed,
            QuestionError::TooLarge,
            QuestionError::Malformed,
            QuestionError::UnsupportedEvent,
            QuestionError::MissingSessionId,
            QuestionError::MissingQuestions,
            QuestionError::MissingQuestionText,
        ] {
            assert!(!error.category().contains('<'));
            assert!(!error.category().contains('{'));
        }
    }

    #[test]
    fn html_and_text_stay_within_format_limits() {
        let long = "y".repeat(20000);
        let json = format!(
            r#"{{"event":"blocked","question":{{"session_id":"ses_1","session_title":"{long}","questions":[{{
                "text":"{long}","options":[{{"label":"{long}","description":"{long}"}}]
            }}]}}}}"#
        );
        let intent = parse(&json).to_intent();
        assert!(intent.body.chars().count() <= crate::notifications::QUESTION_BODY_LIMIT);
        let html = intent.html.expect("html");
        assert!(html.len() <= crate::notifications::QUESTION_HTML_LIMIT_BYTES);
    }
}
