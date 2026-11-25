//! The `logs` command: a bounded, read-only view of the diagnostic log.
//!
//! The command is a utility: it prepares no configuration, resolves no
//! provider, uses no credential, and touches no registry. It only reads the
//! owned log files, and it writes nothing at all.

use crate::command::{Command, HelpTopic};

/// The default number of events a query returns.
pub(crate) const DEFAULT_LIMIT: usize = 50;
/// The largest number of events a query returns.
pub(crate) const MAX_LIMIT: usize = 500;

/// Whether one token is the help request.
fn help_flag(value: &str) -> bool {
    value == "--help" || value == "-h"
}

/// A parsed `logs` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsCommand {
    /// The exact run identifier to filter for.
    pub run: Option<String>,
    /// The largest number of events to return.
    pub limit: Option<usize>,
    /// Whether only the resolved log directory and its policy are reported.
    pub path: bool,
}

/// Parse the `logs` arguments.
pub(crate) fn parse_logs(rest: &[String]) -> Result<Command, String> {
    if rest.first().map(String::as_str).is_some_and(help_flag) {
        return Ok(Command::Help(HelpTopic::Logs));
    }
    let mut run: Option<String> = None;
    let mut limit: Option<usize> = None;
    let mut path = false;
    let mut index = 0;
    while index < rest.len() {
        let token = rest[index].as_str();
        match token {
            "--path" => {
                if path {
                    return Err("--path must not be given twice".to_owned());
                }
                path = true;
                index += 1;
            }
            "--run" => {
                if run.is_some() {
                    return Err("--run must not be given twice".to_owned());
                }
                let value = super::parse_helpers::required_value(rest, index, "--run")?;
                run = Some(validate_run_id(&value)?);
                index += 2;
            }
            "--limit" => {
                if limit.is_some() {
                    return Err("--limit must not be given twice".to_owned());
                }
                let value = super::parse_helpers::required_value(rest, index, "--limit")?;
                limit = Some(validate_limit(&value)?);
                index += 2;
            }
            value if super::parse_helpers::split_inline(value, "--run").is_some() => {
                if run.is_some() {
                    return Err("--run must not be given twice".to_owned());
                }
                let value = super::parse_helpers::split_inline(value, "--run")
                    .expect("the inline form was just matched");
                run = Some(validate_run_id(&value)?);
                index += 1;
            }
            value if super::parse_helpers::split_inline(value, "--limit").is_some() => {
                if limit.is_some() {
                    return Err("--limit must not be given twice".to_owned());
                }
                let value = super::parse_helpers::split_inline(value, "--limit")
                    .expect("the inline form was just matched");
                limit = Some(validate_limit(&value)?);
                index += 1;
            }
            value => return Err(format!("unknown option '{value}'")),
        }
    }
    if path && (run.is_some() || limit.is_some()) {
        return Err("--path cannot be combined with --run or --limit".to_owned());
    }
    Ok(Command::Logs(LogsCommand { run, limit, path }))
}

/// Whether the value is a safe run identifier: 1..=128 ASCII characters of
/// letters, digits, dot, underscore, colon, or hyphen.
fn validate_run_id(value: &str) -> Result<String, String> {
    if crate::infra::diag_log::is_safe_token(value) {
        Ok(value.to_owned())
    } else {
        Err("--run must be 1..128 characters of letters, digits, '.', '_', ':', or '-'".to_owned())
    }
}

/// Whether the value is a bounded entry count.
fn validate_limit(value: &str) -> Result<usize, String> {
    let parsed: usize = value
        .trim()
        .parse()
        .map_err(|_| "--limit must be a number between 1 and 500".to_owned())?;
    if parsed == 0 || parsed > MAX_LIMIT {
        return Err("--limit must be a number between 1 and 500".to_owned());
    }
    Ok(parsed)
}
