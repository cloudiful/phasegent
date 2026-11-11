//! The local, Git-only guards a registered worktree directory must pass.
//!
//! These are the pre-existing removal rules: never the main checkout, never
//! dirty or untracked. Current occupancy is decided by the connected host, not
//! by registration, so no registration row can veto a removal here. Each
//! failure returns a bounded reason naming the guard so the operator can act
//! on it; the caller prefixes it with the issue and appends the directory
//! verbatim.

use std::path::{Path, PathBuf};

use crate::worktree::{CanonicalPaths, WorktreeError, WorktreeRunner, cache::CacheInventory};

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

/// Evaluate the local guards for one candidate directory. A protected reason
/// means the directory must be kept; otherwise the carried cache assessment is
/// the only dirty state the shared removal may clear.
pub(super) fn evaluate(
    runner: &dyn WorktreeRunner,
    identity: &str,
    directory: &Path,
) -> LocalGuard {
    CanonicalPaths::default().evaluate_guards(runner, identity, directory)
}

impl CanonicalPaths {
    pub(crate) fn evaluate_guards(
        &self,
        runner: &dyn WorktreeRunner,
        identity: &str,
        directory: &Path,
    ) -> LocalGuard {
        match is_main_checkout_with_paths(runner, directory, identity, self) {
            Ok(true) => return LocalGuard::protected("main checkout is never removed"),
            Ok(false) => {}
            Err(error) => {
                return LocalGuard::protected(format!(
                    "could not verify the main-checkout guard: {}",
                    crate::lifecycle::bounded(&error.message)
                ));
            }
        }
        match crate::worktree::is_clean(runner, directory) {
            Ok(true) => LocalGuard::allowed(),
            Ok(false) => match crate::worktree::cache::inspect(runner, directory) {
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
        directory: &Path,
    ) -> Option<String> {
        self.evaluate_guards(runner, identity, directory)
            .protected_reason
    }
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
