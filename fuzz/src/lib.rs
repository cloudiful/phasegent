//! In-process fuzz harness for the argv walker in `src/command/argv.rs`.
//!
//! The library compiles the shipped binary source (`include!` of
//! `../src/main.rs`) and exposes the frozen `&[u8] -> parser` entry point
//! `parse_argv_bytes`. The entry must never panic; a panic escaping it is the
//! fuzz finding. `command::parse` is the crate's public parse entry; the
//! `PHASEGENT_ROLE` fallback is modeled by setting the variable around the
//! call, so the fuzzed code path is the shipped one.
//!
//! Corpus format: NUL-separated argv tokens — the same slice `argv::parse`
//! receives from `main` (no program name). A trailing separator is not an empty
//! token and an empty input is zero tokens (`phasegent` with no arguments). An
//! optional first token `env:<value>` models the `PHASEGENT_ROLE` environment
//! variable and is not passed to the parser; a bare `env:` models a blank value.
#![allow(dead_code)]
#![allow(unexpected_cfgs)]

include!("../../src/main.rs");

/// Drive the shipped parser over one corpus input.
///
/// `Ok(tag)` names the parsed top-level command (the outcome class),
/// `Err(message)` is the parser's own error text.
pub fn parse_argv_bytes(input: &[u8]) -> Result<String, String> {
    let mut tokens = Vec::new();
    let mut role_env: Option<String> = None;
    for (index, raw) in split_tokens(input).into_iter().enumerate() {
        let token = String::from_utf8_lossy(raw).into_owned();
        if index == 0 && token.starts_with("env:") {
            role_env = Some(token["env:".len()..].to_owned());
            continue;
        }
        tokens.push(token);
    }
    let outcome = match role_env {
        None => command::parse(&tokens),
        Some(value) => with_role_env(&value, || command::parse(&tokens)),
    };
    match outcome {
        Ok(invocation) => Ok(command_tag(&invocation.command)),
        Err(message) => Err(message),
    }
}

/// Split a corpus input into argv tokens: NUL is the separator, a trailing
/// separator creates no trailing empty token, and an empty input is zero
/// tokens (`phasegent` with no arguments).
fn split_tokens(input: &[u8]) -> Vec<&[u8]> {
    if input.is_empty() {
        return Vec::new();
    }
    let mut parts: Vec<&[u8]> = input.split(|byte| *byte == 0).collect();
    if input.ends_with(&[0]) {
        parts.pop();
    }
    parts
}

/// Run `parse` with `PHASEGENT_ROLE` set. The driver and the libFuzzer target
/// are single-threaded and the shipped parser spawns no threads, so the
/// process-global mutation cannot race a concurrent read.
fn with_role_env<T>(value: &str, run: impl FnOnce() -> T) -> T {
    // SAFETY: single-threaded caller, as documented on this function.
    unsafe { std::env::set_var("PHASEGENT_ROLE", value) };
    let result = run();
    // SAFETY: single-threaded caller, as documented on this function.
    unsafe { std::env::remove_var("PHASEGENT_ROLE") };
    result
}

/// Outcome class for a parsed command, e.g. `Issue/Create`, `Help/Worktree`.
fn command_tag(command: &command::Command) -> String {
    let debug = format!("{command:?}");
    let mut parts = debug.split(['(', '{', ' ', ',', ')']).filter(|part| {
        part.chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase())
    });
    match (parts.next(), parts.next()) {
        (Some(first), Some(second)) => format!("{first}/{second}"),
        (Some(first), None) => first.to_owned(),
        _ => "Unknown".to_owned(),
    }
}

/// Outcome class for a parser error: the message without its dynamic operands.
pub fn error_tag(message: &str) -> String {
    let end = message
        .find(['\'', '"', '`'])
        .unwrap_or(message.len())
        .min(message.len());
    message[..end].trim_end().to_owned()
}
