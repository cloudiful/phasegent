//! Where a closing session is allowed to go.
//!
//! A move request names one directory, and naming the wrong one would
//! strand a session in a directory this pass is about to delete. The
//! target is therefore resolved from Git's own authoritative worktree
//! list and then verified: the entry must be inside a work tree and must
//! be the checkout whose per-worktree Git dir is the repository's shared
//! common dir. Anything else — a bare repository, a vanished listing, an
//! entry Git cannot confirm — leaves the candidate associated and kept.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use super::guards::is_main_checkout;
use crate::worktree::{WorktreeRunner, parse_worktree_list};

/// A verified main checkout, or the reason no target could be proven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MoveTarget {
    Main(PathBuf),
    Unavailable(String),
}

impl MoveTarget {
    /// The verified main checkout, or the bounded reason there is none.
    pub(crate) fn resolve(&self) -> Result<&Path, &str> {
        match self {
            Self::Main(path) => Ok(path),
            Self::Unavailable(reason) => Err(reason.as_str()),
        }
    }
}

/// Resolve `repo_path`'s main checkout through `git worktree list
/// --porcelain`. Git lists the main worktree first, but the ordering is
/// not the proof: every entry is verified before it is accepted.
///
/// `runner` must be [`crate::worktree::RawOutputRunner`] in production so
/// the porcelain records survive; tests inject their own.
pub(crate) fn resolve_main_checkout(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    identity: &str,
) -> MoveTarget {
    if identity.is_empty() {
        return MoveTarget::Unavailable("the repository identity could not be resolved".to_owned());
    }
    let listing = match runner.run(&["worktree", "list", "--porcelain"], repo_path) {
        Ok(listing) if listing.status == 0 => listing.stdout,
        Ok(listing) => {
            return MoveTarget::Unavailable(format!(
                "git worktree list failed with exit status {}",
                listing.status
            ));
        }
        Err(error) => return MoveTarget::Unavailable(crate::lifecycle::bounded(&error.message)),
    };
    let entries = parse_worktree_list(&listing);
    if entries.is_empty() {
        return MoveTarget::Unavailable("git worktree list reported no worktree".to_owned());
    }
    for entry in entries {
        let candidate = PathBuf::from(&entry.worktree);
        if !candidate.is_dir() {
            continue;
        }
        let inside = match crate::worktree::is_inside_work_tree(runner, &candidate) {
            Ok(true) => true,
            Ok(false) => false,
            Err(_) => false,
        };
        if !inside {
            continue;
        }
        if is_main_checkout(runner, &candidate, identity).unwrap_or(false) {
            return MoveTarget::Main(candidate);
        }
    }
    MoveTarget::Unavailable("no main checkout could be verified".to_owned())
}

/// Lazily resolved move target for one cleanup pass.
///
/// Reading the worktree listing spawns a process, so it must not happen
/// for a pass that never contemplates a move: a pure CLI close, a
/// reconciliation pass that may only look, and a read-only report all
/// resolve nothing at all. At most one listing is read per pass.
pub(crate) struct TargetResolver<'a> {
    resolved: OnceCell<MoveTarget>,
    repo_path: &'a Path,
    identity: &'a str,
}

impl<'a> TargetResolver<'a> {
    pub(crate) fn new(repo_path: &'a Path, identity: &'a str) -> Self {
        Self {
            resolved: OnceCell::new(),
            repo_path,
            identity,
        }
    }

    /// The target, resolving it on first use.
    pub(crate) fn get(&self) -> &MoveTarget {
        self.resolved.get_or_init(|| {
            resolve_main_checkout(
                &crate::worktree::RawOutputRunner,
                self.repo_path,
                self.identity,
            )
        })
    }

    /// A resolver that already knows its answer, so a caller that only
    /// exercises the decision table never spawns Git.
    #[cfg(test)]
    pub(crate) fn resolved(target: MoveTarget) -> Self {
        let cell = OnceCell::new();
        let _ = cell.set(target);
        Self {
            resolved: cell,
            repo_path: Path::new("."),
            identity: "",
        }
    }
}
