//! The local, Git-only guards a closed issue's candidate must pass.
//!
//! These are the pre-existing removal rules, unchanged: never the main
//! checkout, never a directory another session holds an `active` lease
//! on, never dirty. Each returns a bounded reason naming the guard so the
//! operator can act on it; the caller prefixes it with the issue and
//! appends the directory verbatim.

use std::path::{Path, PathBuf};

use crate::worktree::{
    LEASE_STATUS_ACTIVE, LeaseRow, WorktreeError, WorktreeRunner, canonical_or_self, same_directory,
};

/// Evaluate the local guards for one candidate row. `None` means the
/// directory passed every guard and may be removed.
pub(super) fn evaluate(
    runner: &dyn WorktreeRunner,
    rows: &[LeaseRow],
    identity: &str,
    session: Option<&str>,
    row: &LeaseRow,
) -> Option<String> {
    let directory = PathBuf::from(&row.worktree_path);
    match is_main_checkout(runner, &directory, identity) {
        Ok(true) => return Some("main checkout is never removed".to_owned()),
        Ok(false) => {}
        Err(error) => {
            return Some(format!(
                "could not verify the main-checkout guard: {}",
                crate::lifecycle::bounded(&error.message)
            ));
        }
    }
    if let Some(other) = foreign_active_lease(rows, session, row) {
        return Some(format!(
            "session '{}' holds an active lease for issue {}",
            other.session, other.issue
        ));
    }
    match crate::worktree::is_clean(runner, &directory) {
        Ok(true) => None,
        Ok(false) => Some("uncommitted or untracked files".to_owned()),
        Err(error) => Some(format!(
            "cleanliness probe failed: {}",
            crate::lifecycle::bounded(&error.message)
        )),
    }
}

/// An `active` row pointing at the same directory from another session.
/// The close chain flips this issue's `active` leases to `retained`
/// before the cleanup runs, so an `active` row seen here belongs to
/// another issue or to another session; the closing session's own row is
/// exempted for the callers that reach the guard without a preceding
/// flip, such as the `issue sync` report mode. `session == None` makes
/// every still-active row foreign, which keeps an unconverged directory
/// instead of deleting it.
fn foreign_active_lease<'a>(
    rows: &'a [LeaseRow],
    session: Option<&str>,
    row: &LeaseRow,
) -> Option<&'a LeaseRow> {
    let directory = Path::new(&row.worktree_path);
    rows.iter().find(|other| {
        other.status == LEASE_STATUS_ACTIVE
            && session != Some(other.session.as_str())
            && same_directory(Path::new(&other.worktree_path), directory)
    })
}

/// True when `directory` is the repository's main working tree: the
/// checkout whose per-worktree Git dir *is* the shared common dir. Every
/// linked worktree resolves `git rev-parse --git-dir` to
/// `<common>/worktrees/<name>` instead, so it never matches. A probe that
/// cannot answer is an `Err`; callers must then keep the directory,
/// because the guard cannot be verified.
pub(super) fn is_main_checkout(
    runner: &dyn WorktreeRunner,
    directory: &Path,
    identity: &str,
) -> Result<bool, WorktreeError> {
    let output = runner.run(&["rev-parse", "--git-dir"], directory)?;
    if output.status != 0 {
        return Err(WorktreeError::new(
            "git",
            format!(
                "git rev-parse --git-dir failed with exit status {}",
                output.status
            ),
        ));
    }
    let raw = output.stdout.trim();
    if raw.is_empty() {
        return Err(WorktreeError::new(
            "git",
            "git rev-parse --git-dir returned an empty path",
        ));
    }
    Ok(canonical_or_self(&resolve_against(directory, raw))
        == canonical_or_self(Path::new(identity)))
}

fn resolve_against(base: &Path, raw: &str) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
}
