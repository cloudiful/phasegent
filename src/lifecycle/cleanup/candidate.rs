//! The one candidate-protection check every removal path shares.
//!
//! `issue close`, `issue sync`, and `worktree prune` all contemplate deleting
//! a leased worktree directory, and none of them may relax the others' guards.
//! This module is the single evaluation of those guards, extracted from the
//! closed-issue cleanup so `worktree prune` reuses the exact same rules instead
//! of a parallel copy:
//!
//! 1. the local Git guards — never the main checkout, never a directory an
//!    `active` lease points at (including the caller's own active row when no
//!    closing session is named), never dirty or untracked;
//! 2. the OpenCode host guard — for a directory a persisted `ses_`-leased
//!    session still occupies, the host is the authority and an unreadable,
//!    incomplete, or unresolvable answer keeps the directory.
//!
//! `mode` decides who may act on the host's answer. [`CleanupMode::Close`]
//! and [`CleanupMode::Prune`] may return the candidate's idle lease family
//! to the verified main checkout; the reconciliation pass
//! ([`CleanupMode::Sync`]) never moves a session, so a directory that is
//! still occupied is kept for a later pass; and [`CleanupMode::Preview`]
//! reports what a removal would do without touching anything.

use std::path::Path;

use crate::worktree::LeaseRow;
use crate::worktree::WorktreeRunner;
use crate::worktree::opencode::OpenCodeApi;

use super::CleanupMode;
use super::target::TargetResolver;
use super::{guards, sessions};

/// One source of the host half of the candidate guard.
///
/// The direct source is the production default and preserves the
/// close/report/prune behaviour exactly. The sync pass supplies a
/// read-through source that may answer from an invocation-local memo but
/// must re-evaluate a would-be-free verdict through the direct source
/// before a directory is removed.
pub(crate) trait OccupancyGuard {
    fn keep_reason(
        &self,
        mode: CleanupMode,
        target: &TargetResolver<'_>,
        directory: &Path,
        associated: &[String],
        closing: Option<&str>,
    ) -> Option<String>;
}

/// The direct, uncached host guard. Every caller that is not the optimized
/// reconciliation pass uses this, so their semantics are unchanged.
pub(crate) struct DirectOccupancy<'a>(pub(crate) &'a dyn OpenCodeApi);

impl OccupancyGuard for DirectOccupancy<'_> {
    fn keep_reason(
        &self,
        mode: CleanupMode,
        target: &TargetResolver<'_>,
        directory: &Path,
        associated: &[String],
        closing: Option<&str>,
    ) -> Option<String> {
        occupancy_reason(self.0, mode, target, directory, associated, closing)
    }
}

/// The single OpenCode occupancy predicate behind every guard source.
///
/// `None` means the host evidence allows removal; a `Some` value is the
/// bounded reason the directory is kept. Keeping this in one place is what
/// lets an optimized source repeat the *same* predicate against a direct
/// API before it authorizes a removal.
pub(crate) fn occupancy_reason(
    api: &dyn OpenCodeApi,
    mode: CleanupMode,
    target: &TargetResolver<'_>,
    directory: &Path,
    associated: &[String],
    closing: Option<&str>,
) -> Option<String> {
    sessions::finalize_family(api, mode, target, directory, associated, closing).keep_reason()
}

/// The local, Git-only half of the candidate guard: never the main checkout,
/// never an `active` peer's path, never dirty or untracked. It runs no host or
/// provider read, so a caller that must decide a directory's fate before
/// touching the host — `worktree prune` classifies before it plans child
/// retirement — can use it to exclude a main/dirty path with zero host I/O. It
/// is the same `guards::evaluate` call the host guard is layered on, not a
/// parallel copy, so the two can never disagree. `closing` preserves the
/// close/sync caller's own-session exemption; prune passes `None`.
#[allow(dead_code)]
pub(crate) fn local_protected_reason(
    runner: &dyn WorktreeRunner,
    identity: &str,
    rows: &[LeaseRow],
    closing: Option<&str>,
    row: &LeaseRow,
) -> Option<String> {
    guards::evaluate(runner, rows, identity, closing, row).protected_reason
}

/// The full local guard outcome: a protected reason or the exact root `.cache`
/// assessment to carry through the later removal.
pub(crate) fn local_guard(
    runner: &dyn WorktreeRunner,
    identity: &str,
    rows: &[LeaseRow],
    closing: Option<&str>,
    row: &LeaseRow,
) -> guards::LocalGuard {
    guards::evaluate(runner, rows, identity, closing, row)
}
