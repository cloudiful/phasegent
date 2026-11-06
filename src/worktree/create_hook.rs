//! Auto-acquire hook for `issue create`.
//!
//! A successful create is the point where a session's task identity
//! becomes known, so the caller runs this helper to let the shared
//! conflict table decide whether the current checkout can be reused or a
//! conflict needs explicit isolation. The helper always runs the table
//! with the reuse preference (issue 651 P3): it never creates a
//! worktree by itself — an occupied checkout path resolves to an
//! actionable `isolation` warning naming `phasegent worktree acquire
//! --issue N --isolate`. It reuses [`resolve_session`] +
//! [`acquire_lease_with`] unchanged, never fails the caller (a missing
//! session, unreadable storage, or git error degrades to silence), and
//! reports a created worktree as a bounded warning string the caller
//! forwards through `cli::report_local_warnings`. Stdout JSON is never
//! touched, and no branch or worktree is deleted.

use std::path::PathBuf;

use crate::worktree::{AcquireOptions, ProcessWorktreeRunner, acquire_lease_with, resolve_session};

use super::acquire::is_isolation_required;

/// Best-effort worktree acquisition after `issue create`.
///
/// Returns `None` when there is nothing to report (no session, an acquire
/// error that is not isolation guidance, or a silent reuse of the current
/// checkout) and otherwise the bounded stderr warning string. A created
/// worktree appends the `reason=new_worktree` redirect notice so the
/// operator knows later tool calls land there; a refused conflict returns
/// the actionable `isolation` guidance instead, and the create itself
/// stays successful either way.
///
/// The production create path never calls this form any more: every
/// successful create acquires under its durable task scope or skips. It
/// remains the unscoped entry point the worktree hook tests drive directly.
#[allow(dead_code)]
pub(crate) fn auto_acquire_after_create(
    issue: u64,
    explicit_session: Option<&str>,
) -> Option<String> {
    auto_acquire_after_create_scoped(issue, explicit_session, None)
}

/// The scoped form of [`auto_acquire_after_create`]. Every routed create
/// names its exact durable task scope, so the hook writes a sidecar-scoped
/// lease and never falls back to an unscoped one when the scope, identity, or
/// store cannot be proven (a best-effort silence).
pub(crate) fn auto_acquire_after_create_scoped(
    issue: u64,
    explicit_session: Option<&str>,
    scope: Option<&crate::worktree::LeaseScope>,
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
    let options = AcquireOptions {
        cache_base: None,
        isolate: false,
        base: None,
        // The implicit hook states the reuse preference: it books the
        // current checkout when its path is free and reports isolation
        // guidance when it is occupied. It never creates a worktree.
        reuse: true,
    };
    let outcome = match scope {
        Some(scope) => {
            let storage = match crate::infra::storage::Storage::open() {
                Ok(storage) => storage,
                Err(_) => return None,
            };
            if crate::worktree::leases::ensure_schema(&storage).is_err() {
                return None;
            }
            let identity = match crate::worktree::repo_identity(&runner, &repo_path) {
                Ok(identity) => identity,
                Err(_) => return None,
            };
            crate::worktree::acquire_lease_scoped(
                &runner,
                &storage,
                &identity,
                &repo_path,
                &session.id,
                options,
                scope,
            )
        }
        None => acquire_lease_with(&runner, &repo_path, issue, &session.id, options),
    };
    match outcome {
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
        Err(error) if is_isolation_required(&error) => Some(error.message),
        Err(_) => None,
    }
}
