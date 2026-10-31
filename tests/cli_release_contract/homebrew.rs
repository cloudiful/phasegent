//! Homebrew tap bump workflow: the bump job runs only for a successful
//! upstream Release run, while its release and manual dispatch paths are
//! unaffected. The job-level `if` expression is read from the real workflow and
//! evaluated against an event matrix, so the test follows the condition that
//! GitHub actually applies instead of restating it.

use std::path::PathBuf;

fn bump_yml() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join(".github/workflows/bump-homebrew-tap.yml"))
        .unwrap_or_else(|err| panic!("read bump workflow: {err}"))
}

fn job_block(yml: &str, job: &str) -> String {
    let header = format!("\n  {job}:\n");
    let start = yml
        .find(&header)
        .unwrap_or_else(|| panic!("bump workflow must keep job {job}; got:\n{yml}"))
        + header.len();
    let mut block = String::new();
    for line in yml[start..].lines() {
        if !line.is_empty() && !line.starts_with(' ') {
            break;
        }
        block.push_str(line);
        block.push('\n');
    }
    block
}

fn job_condition(block: &str, job: &str) -> String {
    for line in block.lines() {
        let Some(value) = line.trim_start().strip_prefix("if:") else {
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        assert_eq!(
            indent, 4,
            "job {job} must gate the whole job with a job-level if, not a step condition"
        );
        let value = value.trim();
        assert!(
            !value.is_empty() && !value.starts_with(['|', '>']),
            "job {job} if must stay a single-line expression, got {value:?}"
        );
        return value.to_string();
    }
    panic!("job {job} must carry a job-level if condition");
}

#[derive(Clone, Copy, Debug)]
struct Event<'a> {
    name: &'a str,
    conclusion: Option<&'a str>,
}

#[derive(Debug, PartialEq)]
enum Token {
    Ident(String),
    Str(String),
    Eq,
    Ne,
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn tokenize(expr: &str) -> Vec<Token> {
    let chars: Vec<char> = expr.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        match (ch, next) {
            ('|', Some('|')) => {
                tokens.push(Token::Or);
                i += 2;
            }
            ('&', Some('&')) => {
                tokens.push(Token::And);
                i += 2;
            }
            ('=', Some('=')) => {
                tokens.push(Token::Eq);
                i += 2;
            }
            ('!', Some('=')) => {
                tokens.push(Token::Ne);
                i += 2;
            }
            ('!', _) => {
                tokens.push(Token::Not);
                i += 1;
            }
            ('(', _) => {
                tokens.push(Token::LParen);
                i += 1;
            }
            (')', _) => {
                tokens.push(Token::RParen);
                i += 1;
            }
            ('\'', _) => {
                let rest = &chars[i + 1..];
                let end = rest
                    .iter()
                    .position(|&c| c == '\'')
                    .unwrap_or_else(|| panic!("unterminated string literal in {expr:?}"));
                tokens.push(Token::Str(rest[..end].iter().collect()));
                i += end + 2;
            }
            (c, _) if c.is_ascii_alphanumeric() || c == '_' || c == '.' => {
                let end = chars[i..]
                    .iter()
                    .position(|&c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
                    .unwrap_or(chars.len() - i);
                tokens.push(Token::Ident(chars[i..i + end].iter().collect()));
                i += end;
            }
            _ => panic!("unsupported character {ch:?} in condition {expr:?}"),
        }
    }
    tokens
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    event: Event<'a>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn parse_or(&mut self) -> bool {
        let mut value = self.parse_and();
        while self.peek() == Some(&Token::Or) {
            self.pos += 1;
            let right = self.parse_and();
            value = value || right;
        }
        value
    }

    fn parse_and(&mut self) -> bool {
        let mut value = self.parse_unary();
        while self.peek() == Some(&Token::And) {
            self.pos += 1;
            let right = self.parse_unary();
            value = value && right;
        }
        value
    }

    fn parse_unary(&mut self) -> bool {
        match self.peek() {
            Some(Token::Not) => {
                self.pos += 1;
                !self.parse_unary()
            }
            Some(Token::LParen) => {
                self.pos += 1;
                let value = self.parse_or();
                assert_eq!(self.peek(), Some(&Token::RParen), "unbalanced parenthesis");
                self.pos += 1;
                value
            }
            _ => self.parse_comparison(),
        }
    }

    fn parse_comparison(&mut self) -> bool {
        let Some(Token::Ident(path)) = self.peek() else {
            panic!(
                "condition must compare a context value; got {:?}",
                self.peek()
            );
        };
        let path = path.clone();
        self.pos += 1;
        let op = match self.peek() {
            Some(Token::Eq) => Token::Eq,
            Some(Token::Ne) => Token::Ne,
            other => panic!("unsupported operator {other:?} for {path}"),
        };
        self.pos += 1;
        let Some(Token::Str(literal)) = self.peek() else {
            panic!("{path} must be compared against a quoted literal");
        };
        let literal = literal.clone();
        self.pos += 1;
        let actual = self.resolve(&path);
        match op {
            Token::Eq => actual == Some(literal.as_str()),
            _ => actual != Some(literal.as_str()),
        }
    }

    fn resolve(&self, path: &str) -> Option<&str> {
        match path {
            "github.event_name" => Some(self.event.name),
            "github.event.workflow_run.conclusion" => self.event.conclusion,
            other => panic!("condition reads untested context value {other:?}"),
        }
    }
}

fn evaluate(expr: &str, event: Event<'_>) -> bool {
    let tokens = tokenize(expr);
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        event,
    };
    let value = parser.parse_or();
    assert_eq!(
        parser.pos,
        tokens.len(),
        "unconsumed tokens in condition {expr:?}"
    );
    value
}

#[test]
fn bump_job_skips_every_unsuccessful_release_run_before_steps() {
    let yml = bump_yml();
    let block = job_block(&yml, "bump");
    let condition = job_condition(&block, "bump");
    let steps = block
        .find("\n    steps:")
        .unwrap_or_else(|| panic!("bump job must keep its steps"));
    let gate = block
        .find("if:")
        .expect("bump job must carry a job-level if");
    assert!(
        gate < steps,
        "the gate must be a job-level condition so no runner is allocated for a skipped run"
    );

    let matrix = [
        (
            Event {
                name: "workflow_run",
                conclusion: Some("success"),
            },
            true,
        ),
        (
            Event {
                name: "workflow_run",
                conclusion: Some("failure"),
            },
            false,
        ),
        (
            Event {
                name: "workflow_run",
                conclusion: Some("cancelled"),
            },
            false,
        ),
        (
            Event {
                name: "workflow_run",
                conclusion: Some("skipped"),
            },
            false,
        ),
        (
            Event {
                name: "workflow_run",
                conclusion: Some("timed_out"),
            },
            false,
        ),
        (
            Event {
                name: "release",
                conclusion: None,
            },
            true,
        ),
        (
            Event {
                name: "workflow_dispatch",
                conclusion: None,
            },
            true,
        ),
    ];
    for (event, expected) in matrix {
        assert_eq!(
            evaluate(&condition, event),
            expected,
            "job condition {condition:?} must evaluate to {expected} for {event:?}"
        );
    }
}

#[test]
fn bump_job_drops_the_duplicate_step_level_conclusion_gate() {
    let yml = bump_yml();
    let block = job_block(&yml, "bump");
    let condition = job_condition(&block, "bump");
    assert_eq!(
        yml.matches("workflow_run.conclusion").count(),
        condition.matches("workflow_run.conclusion").count(),
        "the job-level condition must be the only place that reads the conclusion; got:\n{yml}"
    );
    assert!(
        !block.contains("conclusion is"),
        "the Resolve tag step must not repeat the conclusion skip; got:\n{block}"
    );
    assert!(
        !yml.contains("non-success conclusion"),
        "the Assert tag shape message must describe only the reachable non-release branch; got:\n{yml}"
    );
    assert!(
        yml.contains("(non-release branch), skipping quietly."),
        "the Assert tag shape message must keep the live non-release-branch case; got:\n{yml}"
    );
}

#[test]
fn bump_job_keeps_its_triggers_release_steps_and_permissions() {
    let yml = bump_yml();
    for required in [
        "  release:\n    types: [published]",
        "  workflow_run:\n    workflows: [\"Release\"]\n    types: [completed]",
        "  workflow_dispatch:",
        "  group: homebrew-bump-${{ github.event.release.tag_name || github.event.workflow_run.head_branch || inputs.tag }}",
        "  cancel-in-progress: false",
        "    permissions:\n      contents: read",
        "        if: steps.tag.outputs.value != ''",
        "name: Commit and push",
    ] {
        assert!(
            yml.contains(required),
            "bump workflow must keep {required:?}; got:\n{yml}"
        );
    }
}
