//! Auto-acquire hook shared by `issue create` and `issue bind`.
//!
//! A successful create/bind is the point where a session's task identity
//! becomes known, so the caller runs this helper to let the shared
//! conflict table decide whether the current checkout can be reused or a
//! conflict needs an isolated worktree. It reuses
//! [`resolve_session`] + [`acquire_lease`] unchanged, never fails the
//! caller (a missing session, unreadable storage, or git error degrades
//! to silence), and reports a created worktree as a bounded warning
//! string the caller forwards through `cli::report_local_warnings`.
//! Stdout JSON is never touched, and no branch or worktree is deleted.

use std::path::PathBuf;

use crate::infra::storage::Storage;
use crate::worktree::{
    ProcessWorktreeRunner, acquire_lease, resolve_session, resolve_worktree_auto,
};

/// Best-effort worktree acquisition after `issue create` / `issue bind`.
///
/// Returns `None` when there is nothing to report (no session, an acquire
/// error, or a silent reuse of the current checkout) and otherwise the
/// bounded stderr warning string. A created worktree appends the
/// `reason=new_worktree` redirect notice so the operator knows later tool
/// calls land there.
pub(crate) fn auto_acquire_after_bind(
    issue: u64,
    explicit_session: Option<&str>,
) -> Option<String> {
    if issue == 0 {
        return None;
    }
    let session = match resolve_session(explicit_session) {
        Ok(context) => context,
        Err(_) => return None,
    };
    let repo_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let runner = ProcessWorktreeRunner::new();
    let auto = Storage::open()
        .ok()
        .and_then(|storage| resolve_worktree_auto(&storage).ok())
        .unwrap_or(false);
    match acquire_lease(&runner, &repo_path, issue, &session.id, None, false, auto) {
        Ok(outcome) => {
            let mut warnings = outcome.warnings.join("; ");
            if outcome.created {
                let message = format!(
                    "phasegent: acquired worktree {} for issue {} (reason=new_worktree); subsequent tool calls redirect there",
                    outcome.path, issue
                );
                if !warnings.is_empty() {
                    warnings.push_str("; ");
                }
                warnings.push_str(&message);
            }
            if warnings.is_empty() {
                None
            } else {
                Some(warnings)
            }
        }
        Err(_) => None,
    }
}
