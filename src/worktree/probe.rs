//! Read-only worktree probe (issue 595 Phase 1/2).
//!
//! [`probe_path`] answers "what is at this path?" without mutating
//! anything: it never creates or removes a lease, never synchronises a
//! provider, never switches a checkout, and never repairs a branch or a
//! directory. Every failure is captured as a bounded [`ProbeError`] so a
//! non-Git directory, a missing path, or a failing `git` invocation
//! still yields a structured result instead of an error.
//!
//! The probed facts are deliberately small and fixed: existence, whether
//! the path is inside a Git work tree, clean / dirty / unknown, the
//! current branch, `HEAD`, and whether the path is the main checkout.
//! The lease summary is attached by the CLI layer when the selector
//! resolved one; the domain here stays storage-free.

use std::path::Path;

use crate::worktree::git::{
    checkout_git_dirs, current_branch_for, head_rev, is_clean, is_inside_work_tree,
};
use crate::worktree::{WorktreeRunner, bounded};

/// Hard cap on the structured errors one probe reports, so a hostile
/// environment cannot flood the JSON envelope.
pub const MAX_PROBE_ERRORS: usize = 4;

/// One bounded, structured probe failure. `kind` mirrors the
/// [`WorktreeError`] vocabulary (`state` for filesystem facts, `git`
/// for a failing git invocation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    pub kind: &'static str,
    pub message: String,
}

impl ProbeError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: bounded(&message.into()),
        }
    }
}

/// Read-only facts about one path. `clean: None` is the "unknown" state:
/// the `git status` probe failed or the path is not a Git work tree, so
/// the dirty state cannot be trusted. `is_main_checkout: None` means the
/// `--git-dir` comparison could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeFacts {
    pub exists: bool,
    pub is_git_worktree: bool,
    pub clean: Option<bool>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub is_main_checkout: Option<bool>,
    pub errors: Vec<ProbeError>,
}

impl ProbeFacts {
    /// The stable empty result used when an `--issue` selector matches no
    /// lease: no path is guessed, so every fact is absent.
    pub fn empty() -> Self {
        Self {
            exists: false,
            is_git_worktree: false,
            clean: None,
            branch: None,
            head: None,
            is_main_checkout: None,
            errors: Vec::new(),
        }
    }
}

/// Probe `path` read-only. The result is always returned; git and
/// filesystem failures land in [`ProbeFacts::errors`] and never delete,
/// repair, or create anything.
pub fn probe_path(runner: &dyn WorktreeRunner, path: &Path) -> ProbeFacts {
    if !path.exists() {
        return ProbeFacts {
            errors: vec![ProbeError::new(
                "state",
                format!("path does not exist: {}", path.display()),
            )],
            ..ProbeFacts::empty()
        };
    }

    let mut errors: Vec<ProbeError> = Vec::new();
    let is_git_worktree = match is_inside_work_tree(runner, path) {
        Ok(value) => value,
        Err(error) => {
            push_error(&mut errors, "git", error.message.clone());
            false
        }
    };
    if !is_git_worktree {
        push_error(
            &mut errors,
            "state",
            format!("not a Git work tree: {}", path.display()),
        );
        return ProbeFacts {
            exists: true,
            errors,
            ..ProbeFacts::empty()
        };
    }

    let clean = match is_clean(runner, path) {
        Ok(value) => Some(value),
        Err(error) => {
            push_error(
                &mut errors,
                "git",
                format!("dirty probe failed: {}", error.message),
            );
            None
        }
    };
    // A detached HEAD is a legitimate state, not a probe failure, so the
    // structured `branch` error of `current_branch_for` degrades to None.
    let branch = current_branch_for(runner, path).ok();
    let head = match head_rev(runner, path) {
        Ok(value) => value,
        Err(error) => {
            push_error(
                &mut errors,
                "git",
                format!("HEAD probe failed: {}", error.message),
            );
            None
        }
    };
    let is_main_checkout = match checkout_git_dirs(runner, path) {
        Ok((git_dir, common_dir)) => Some(git_dir == common_dir),
        Err(error) => {
            push_error(
                &mut errors,
                "git",
                format!("git-dir probe failed: {}", error.message),
            );
            None
        }
    };

    ProbeFacts {
        exists: true,
        is_git_worktree: true,
        clean,
        branch,
        head,
        is_main_checkout,
        errors,
    }
}

/// Push an error unless the envelope is already at [`MAX_PROBE_ERRORS`].
fn push_error(errors: &mut Vec<ProbeError>, kind: &'static str, message: impl Into<String>) {
    if errors.len() < MAX_PROBE_ERRORS {
        errors.push(ProbeError::new(kind, message));
    }
}
