//! Unit tests for the OpenCode guard of the closed-issue cleanup (issue
//! 747 P1, extended by issue 1195 P1).
//!
//! These drive the decision directly, without a Git or storage fixture:
//! the guard's only inputs are the mode, the candidate directory, the
//! sessions its registry associations name, and the answer the host gives.
//! The full-stack path (real registry rows, real worktree guards, real
//! `git worktree remove`) is user-led integration acceptance.

use std::path::PathBuf;

use super::CleanupMode;
use super::sessions::{FamilyOutcome, finalize_family};
use super::target::{MoveTarget, TargetResolver};
use crate::worktree::opencode::test_support::FakeOpenCodeApi;

pub(super) fn candidate() -> PathBuf {
    PathBuf::from("/tmp/pg-p1-candidate")
}

pub(super) fn main_checkout() -> MoveTarget {
    MoveTarget::Main(PathBuf::from("/tmp/pg-p1-main"))
}

pub(super) fn evaluate_with(
    api: &FakeOpenCodeApi,
    mode: CleanupMode,
    target: &TargetResolver<'_>,
    associated: &[&str],
    closing: Option<&str>,
) -> FamilyOutcome {
    let ids: Vec<String> = associated.iter().map(|id| (*id).to_owned()).collect();
    finalize_family(api, mode, target, &candidate(), &ids, closing)
}

/// A resolver that never touches Git: the fixtures name the target
/// directly so the decision table is exercised without a repository.
pub(super) fn fixed(target: MoveTarget) -> TargetResolver<'static> {
    TargetResolver::resolved(target)
}

#[test]
fn a_candidate_without_an_opencode_association_still_requires_complete_host_evidence() {
    // A pure CLI close is not exempt from the host question: an occupant no
    // registry row names is somebody else's session. An unusable listing is
    // uncertainty and keeps the directory.
    let api = FakeOpenCodeApi::empty().failing_session().failing_list();
    let verdict = evaluate_with(&api, CleanupMode::Close, &fixed(main_checkout()), &[], None);
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an incomplete host answer must keep, got {verdict:?}");
    };
    assert!(reason.contains("incomplete"), "got: {reason}");
    assert!(api.list_calls() > 0, "the host listing must be attempted");
    assert!(api.moves().is_empty());
}
