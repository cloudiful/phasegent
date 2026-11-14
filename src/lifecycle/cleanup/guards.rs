//! The local, Git-only guards a registered worktree directory must pass.
//!
//! These are the pre-existing removal rules: the removal unit is a checkout
//! root, never the main checkout and never a directory nested inside any
//! checkout. Current occupancy is decided by the connected host, not by
//! registration, so no registration row can veto a removal here. Each failure
//! returns a bounded reason naming the guard so the operator can act on it; the
//! caller prefixes it with the issue and appends the directory verbatim.

use std::path::Path;

use crate::worktree::{CanonicalPaths, WorktreeError, WorktreeRunner, cache::CacheInventory};

/// The result of the local Git-only guards for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalGuard {
    pub(crate) protected_reason: Option<String>,
    pub(crate) protected_code: Option<&'static str>,
    /// Present only when the worktree is dirty solely because of an exact root
    /// `.cache`. Callers must carry this assessment forward as the expected
    /// inventory for the later guarded removal.
    pub(crate) cache: Option<CacheInventory>,
}

impl LocalGuard {
    fn allowed() -> Self {
        Self {
            protected_reason: None,
            protected_code: None,
            cache: None,
        }
    }

    fn allowed_cache_only(cache: CacheInventory) -> Self {
        Self {
            protected_reason: None,
            protected_code: None,
            cache: Some(cache),
        }
    }

    fn protected(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            protected_reason: Some(reason.into()),
            protected_code: Some(code),
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
        match checkout_placement(runner, directory, identity, self) {
            Ok(CheckoutPlacement::MainRoot) => {
                return LocalGuard::protected("main_checkout", "main checkout is never removed");
            }
            Ok(CheckoutPlacement::MainSubdirectory) => {
                return LocalGuard::protected(
                    "main_subdirectory",
                    "a directory inside the main checkout is never removed",
                );
            }
            Ok(CheckoutPlacement::LinkedSubdirectory) => {
                return LocalGuard::protected(
                    "checkout_subdirectory",
                    "a directory inside a checkout is never removed",
                );
            }
            Ok(CheckoutPlacement::LinkedRoot) => {}
            Err(error) => {
                let code = if error.kind == "identity" {
                    "foreign_checkout"
                } else {
                    "probe_error"
                };
                return LocalGuard::protected(
                    code,
                    format!(
                        "could not verify the checkout guard: {}",
                        crate::lifecycle::bounded(&error.message)
                    ),
                );
            }
        }
        match crate::worktree::is_clean(runner, directory) {
            Ok(true) => LocalGuard::allowed(),
            Ok(false) => match crate::worktree::cache::inspect(runner, directory) {
                Ok(crate::worktree::cache::CacheInspection::CacheOnly(cache)) => {
                    LocalGuard::allowed_cache_only(cache)
                }
                Ok(crate::worktree::cache::CacheInspection::NotCacheOnly) => {
                    LocalGuard::protected("dirty", "uncommitted or untracked files")
                }
                Err(reason) => LocalGuard::protected("probe_error", reason),
            },
            Err(error) => LocalGuard::protected(
                "probe_error",
                format!(
                    "cleanliness probe failed: {}",
                    crate::lifecycle::bounded(&error.message)
                ),
            ),
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

/// The role a directory plays in its Git checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckoutPlacement {
    /// The repository's main working tree.
    MainRoot,
    /// A directory nested inside the main working tree.
    MainSubdirectory,
    /// The root of a linked worktree: a removable unit.
    LinkedRoot,
    /// A directory nested inside a linked worktree: never a removal unit.
    LinkedSubdirectory,
}

/// Classify `directory` from Git's own answers and attest it against the
/// recorded repository identity. The canonical Git common dir must equal that
/// identity before any root is eligible, so a foreign or replacement checkout
/// at a registered path fails closed. The main checkout is the root whose
/// per-worktree Git dir is the shared common dir; a linked worktree root
/// resolves its Git dir under that common dir instead. Any path whose
/// `--show-toplevel` is not itself is a subdirectory of a checkout, including a
/// subdirectory of a linked worktree, and is never a removable root. A probe
/// that cannot answer is an `Err`; callers must then keep the directory,
/// because the guard cannot be verified.
pub(crate) fn checkout_placement(
    runner: &dyn WorktreeRunner,
    directory: &Path,
    identity: &str,
    paths: &CanonicalPaths,
) -> Result<CheckoutPlacement, WorktreeError> {
    let toplevel = crate::worktree::git::checkout_root(runner, directory)?;
    let (git_dir, common) = crate::worktree::git::checkout_git_dirs(runner, directory)?;
    if git_dir.is_empty() || common.is_empty() {
        return Err(WorktreeError::new(
            "git",
            "git rev-parse returned an empty checkout path",
        ));
    }
    if !paths.same_directory(Path::new(&common), Path::new(identity)) {
        return Err(WorktreeError::new(
            "identity",
            "the checkout does not belong to the expected repository",
        ));
    }
    let is_root = paths.same_directory(&toplevel, directory);
    let is_main = paths.same_directory(Path::new(&git_dir), Path::new(&common));
    Ok(match (is_root, is_main) {
        (true, true) => CheckoutPlacement::MainRoot,
        (false, true) => CheckoutPlacement::MainSubdirectory,
        (true, false) => CheckoutPlacement::LinkedRoot,
        (false, false) => CheckoutPlacement::LinkedSubdirectory,
    })
}

/// True when `directory` is the repository's main working tree. A directory
/// nested inside any checkout, a linked worktree root, and an unverifiable path
/// all answer false: only the main checkout root is a move target.
pub(super) fn is_main_checkout(
    runner: &dyn WorktreeRunner,
    directory: &Path,
    identity: &str,
) -> Result<bool, WorktreeError> {
    Ok(matches!(
        checkout_placement(runner, directory, identity, &CanonicalPaths::default())?,
        CheckoutPlacement::MainRoot
    ))
}
