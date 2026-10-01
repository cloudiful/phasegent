//! Input validation and the clock for the explorer run ledger.
//!
//! Split from the ledger queries because these are the only places a
//! caller-supplied string reaches the database, and the bound on each
//! one is a safety property rather than a query detail.

use std::time::{SystemTime, UNIX_EPOCH};

/// Upper bound for a run label such as an issue/phase tag carried as
/// run metadata.
const MAX_LABEL_CHARS: usize = 128;
/// Upper bound for the worktree path column.
const MAX_CWD_CHARS: usize = 1024;

/// Current wall-clock time in whole seconds, saturating rather than
/// wrapping on a pre-epoch clock.
pub(crate) fn now_epoch_seconds() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

/// Validate a caller-supplied run id: bounded, no control characters.
pub fn validate_run_id(run_id: &str) -> Result<(), String> {
    if run_id.trim().is_empty() || run_id.chars().count() > MAX_LABEL_CHARS {
        return Err(format!(
            "run id must be a non-empty value of at most {MAX_LABEL_CHARS} characters"
        ));
    }
    if run_id.chars().any(char::is_control) {
        return Err("run id must not contain control characters".to_owned());
    }
    Ok(())
}

/// Validate a non-empty worktree path column (server-side data).
pub(crate) fn validate_cwd(cwd: &str) -> Result<(), String> {
    if cwd.trim().is_empty() || cwd.chars().count() > MAX_CWD_CHARS {
        return Err(format!(
            "worktree cwd must be a non-empty path of at most {MAX_CWD_CHARS} characters"
        ));
    }
    if cwd.chars().any(char::is_control) {
        return Err("worktree cwd must not contain control characters".to_owned());
    }
    Ok(())
}
