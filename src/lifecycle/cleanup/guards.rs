//! The local, Git-only guards a closed issue's candidate must pass.
//!
//! These are the pre-existing removal rules, unchanged: never the main
//! checkout, never a directory another session holds an `active` lease
//! on, never dirty. Each returns a bounded reason naming the guard so the
//! operator can act on it; the caller prefixes it with the issue and
//! appends the directory verbatim.

use std::path::{Path, PathBuf};

use crate::worktree::{
    CanonicalPaths, LEASE_STATUS_ACTIVE, LeaseRow, WorktreeError, WorktreeRunner,
    cache::CacheInventory,
};

/// The result of the local Git-only guards for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalGuard {
    pub(crate) protected_reason: Option<String>,
    /// Present only when the worktree is dirty solely because of an exact root
    /// `.cache`. Callers must carry this assessment forward as the expected
    /// inventory for the later guarded removal.
    pub(crate) cache: Option<CacheInventory>,
}

impl LocalGuard {
    fn allowed() -> Self {
        Self {
            protected_reason: None,
            cache: None,
        }
    }

    fn allowed_cache_only(cache: CacheInventory) -> Self {
        Self {
            protected_reason: None,
            cache: Some(cache),
        }
    }

    fn protected(reason: impl Into<String>) -> Self {
        Self {
            protected_reason: Some(reason.into()),
            cache: None,
        }
    }
}

/// Evaluate the local guards for one candidate row. A protected reason means
/// the directory must be kept; otherwise the carried cache assessment is the
/// only dirty state the shared removal may clear.
pub(super) fn evaluate(
    runner: &dyn WorktreeRunner,
    rows: &[LeaseRow],
    identity: &str,
    session: Option<&str>,
    row: &LeaseRow,
) -> LocalGuard {
    CanonicalPaths::default().evaluate_guards(runner, identity, rows, session, row)
}

impl CanonicalPaths {
    pub(crate) fn evaluate_guards(
        &self,
        runner: &dyn WorktreeRunner,
        identity: &str,
        rows: &[LeaseRow],
        session: Option<&str>,
        row: &LeaseRow,
    ) -> LocalGuard {
        let directory = PathBuf::from(&row.worktree_path);
        match is_main_checkout_with_paths(runner, &directory, identity, self) {
            Ok(true) => return LocalGuard::protected("main checkout is never removed"),
            Ok(false) => {}
            Err(error) => {
                return LocalGuard::protected(format!(
                    "could not verify the main-checkout guard: {}",
                    crate::lifecycle::bounded(&error.message)
                ));
            }
        }
        if let Some(other) = foreign_active_lease(rows, session, row, self) {
            return LocalGuard::protected(format!(
                "session '{}' holds an active lease for issue {}",
                other.session, other.issue
            ));
        }
        match crate::worktree::is_clean(runner, &directory) {
            Ok(true) => LocalGuard::allowed(),
            Ok(false) => match crate::worktree::cache::inspect(runner, &directory) {
                Ok(crate::worktree::cache::CacheInspection::CacheOnly(cache)) => {
                    LocalGuard::allowed_cache_only(cache)
                }
                Ok(crate::worktree::cache::CacheInspection::NotCacheOnly) => {
                    LocalGuard::protected("uncommitted or untracked files")
                }
                Err(reason) => LocalGuard::protected(reason),
            },
            Err(error) => LocalGuard::protected(format!(
                "cleanliness probe failed: {}",
                crate::lifecycle::bounded(&error.message)
            )),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn protected_reason(
        &self,
        runner: &dyn WorktreeRunner,
        identity: &str,
        rows: &[LeaseRow],
        session: Option<&str>,
        row: &LeaseRow,
    ) -> Option<String> {
        self.evaluate_guards(runner, identity, rows, session, row)
            .protected_reason
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
    paths: &CanonicalPaths,
) -> Option<&'a LeaseRow> {
    let directory = Path::new(&row.worktree_path);
    rows.iter().find(|other| {
        other.status == LEASE_STATUS_ACTIVE
            && session != Some(other.session.as_str())
            && paths.same_directory(Path::new(&other.worktree_path), directory)
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
    is_main_checkout_with_paths(runner, directory, identity, &CanonicalPaths::default())
}

fn is_main_checkout_with_paths(
    runner: &dyn WorktreeRunner,
    directory: &Path,
    identity: &str,
    paths: &CanonicalPaths,
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
    Ok(paths.same_directory(&resolve_against(directory, raw), Path::new(identity)))
}

fn resolve_against(base: &Path, raw: &str) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
}
