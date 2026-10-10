//! Guarded removal of a closed issue's worktree directories.
//!
//! Three local guards decide a candidate, in this order, and each one
//! keeps the directory with a bounded reason naming itself:
//!
//! 1. it is not the repository's main checkout;
//! 2. no *other* session holds an `active` lease on it;
//! 3. it is clean — `git status --porcelain` is empty, so untracked
//!    files count as dirty;
//! 4. no OpenCode session is still hosted in it (issue 747 P1).
//!
//! The fourth guard is the one that needs the host. A lease records where
//! a session *was*; the OpenCode API reports where it *is*. Only persisted
//! `ses_`-prefixed lease sessions mark a candidate as OpenCode-associated,
//! so a pure CLI close never reaches the API and keeps its previous
//! behaviour exactly.
//!
//! Everything is best-effort: the remote close already succeeded, so an
//! unreachable API, an incomplete listing, an unproven main checkout, or a
//! move that has not taken effect yet all keep the directory and surface
//! one stderr warning. Lease rows are never touched and branches are never
//! deleted.

use std::path::{Path, PathBuf};

#[path = "cleanup/context.rs"]
mod context;
#[path = "cleanup/guards.rs"]
mod guards;
#[path = "cleanup/sessions.rs"]
mod sessions;
#[path = "cleanup/target.rs"]
pub(crate) mod target;

#[cfg(test)]
#[path = "cleanup/tests.rs"]
mod tests;
#[cfg(test)]
#[path = "cleanup/tests_evidence.rs"]
mod tests_evidence;
#[cfg(test)]
#[path = "cleanup/tests_modes.rs"]
mod tests_modes;

pub(crate) use target::TargetResolver;

use crate::worktree::LeaseRow;
use crate::worktree::opencode::{OpenCodeApi, ProcessOpenCodeApi};
use crate::worktree::same_directory;

#[derive(Debug, PartialEq, Eq)]
pub enum AutoCleanupOutcome {
    Noop,
    Cleaned {
        removed: u64,
        kept: Vec<String>,
    },
    /// The repository identity or the lease store could not be resolved.
    /// Nothing was deleted; the remote close already succeeded, so this
    /// is a bounded warning only.
    Warning {
        reason: String,
    },
}

impl AutoCleanupOutcome {
    /// Stderr warning lines: one per preserved directory (reason first,
    /// directory verbatim), or the single warning of the
    /// unresolvable-context arm. Empty for a no-op and for a cleanup
    /// that removed every candidate.
    pub fn warnings(&self) -> Vec<String> {
        match self {
            Self::Noop => Vec::new(),
            Self::Cleaned { kept, .. } => kept.clone(),
            Self::Warning { reason } => vec![super::bounded(reason)],
        }
    }
}

/// How much freedom this pass has to touch an OpenCode session.
///
/// The distinction is a safety boundary, not a performance knob: the
/// reconciliation pass may *look* and never *move*, and the read-only
/// report pass does not probe at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupMode {
    /// `issue close`: may request a move for the closing session only.
    Close,
    /// `issue sync` clean mode: occupancy is checked, sessions are never
    /// moved, so a directory the close could not release stays until a
    /// later pass finds it free.
    Sync,
    /// `issue sync --no-clean`: writes nothing and probes nothing, so an
    /// OpenCode-associated candidate is conservatively reported as kept.
    Report,
}

impl CleanupMode {
    /// Whether this pass may relocate the closing session out of a
    /// candidate. Only the close chain may, and only for its own session.
    pub(crate) fn may_move_sessions(self) -> bool {
        self == Self::Close
    }
}

/// Delete the closed issue's clean worktree directories in this
/// repository, after the remote close and the lease release succeeded.
///
/// The dirty probe ([`crate::worktree::is_clean`]) and the removal call
/// ([`crate::worktree::worktree_remove`]) are the same primitives the
/// `worktree prune --remove` pass is built on; only the guards differ.
/// [`crate::cli::sync`] drives [`cleanup_closed_issue_worktrees_with`]
/// directly with the narrower mode its own pass is allowed.
pub fn cleanup_closed_issue_worktrees(
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
) -> AutoCleanupOutcome {
    cleanup_closed_issue_worktrees_with(
        runner,
        &ProcessOpenCodeApi::new(),
        CleanupMode::Close,
        repo_path,
        issue,
        session,
    )
}

/// Injected core of [`cleanup_closed_issue_worktrees`]. `api` and `mode`
/// are the only knobs the tests turn; every predicate below is the
/// production one.
pub(crate) fn cleanup_closed_issue_worktrees_with(
    runner: &dyn crate::worktree::WorktreeRunner,
    api: &dyn OpenCodeApi,
    mode: CleanupMode,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
) -> AutoCleanupOutcome {
    let context = match context::resolve(runner, repo_path, issue) {
        Ok(context) => context,
        Err(reason) => return AutoCleanupOutcome::Warning { reason },
    };
    if !context.has_issue(issue) {
        return AutoCleanupOutcome::Noop;
    }
    let session = session.map(str::trim).filter(|value| !value.is_empty());
    // The move target is only needed by an OpenCode-associated candidate
    // and only by the close chain, so it is resolved at most once per
    // pass — and never at all for a pure CLI close, a reconciliation
    // pass, or a read-only report.
    let target = TargetResolver::new(repo_path, &context.identity);
    let mut removed = 0u64;
    let mut kept: Vec<String> = Vec::new();
    for row in context.rows.iter().filter(|row| row.issue == issue) {
        let directory = PathBuf::from(&row.worktree_path);
        if !directory.exists() {
            continue;
        }
        let blocked = guards::evaluate(runner, &context.rows, &context.identity, session, row)
            .or_else(|| {
                sessions::evaluate(
                    api,
                    mode,
                    &target,
                    &directory,
                    &associated_sessions(&context.rows, &directory),
                    session,
                )
                .keep_reason()
            });
        if let Some(reason) = blocked {
            kept.push(kept_entry(issue, &reason, &row.worktree_path));
            continue;
        }
        // Run the removal from inside the candidate: it exists at this
        // point, while the close's own working directory may already be
        // a removed sibling by the time a later candidate is handled.
        match crate::worktree::worktree_remove(runner, &directory, &directory) {
            Ok(()) => removed += 1,
            Err(error) => kept.push(kept_entry(
                issue,
                &format!(
                    "worktree removal failed ({})",
                    super::bounded(&error.message)
                ),
                &row.worktree_path,
            )),
        }
    }
    if removed == 0 && kept.is_empty() {
        return AutoCleanupOutcome::Noop;
    }
    AutoCleanupOutcome::Cleaned { removed, kept }
}

/// Distinct OpenCode-managed sessions whose persisted lease points at
/// `directory`, sorted so one pass reports identically every time.
fn associated_sessions(rows: &[LeaseRow], directory: &Path) -> Vec<String> {
    let mut ids: Vec<String> = rows
        .iter()
        .filter(|row| same_directory(Path::new(&row.worktree_path), directory))
        .map(|row| row.session.trim().to_owned())
        .filter(|session| crate::worktree::opencode::is_opencode_session(session))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn kept_entry(issue: u64, reason: &str, directory: &str) -> String {
    format!("issue {issue} closed; kept worktree ({reason}): {directory}")
}
