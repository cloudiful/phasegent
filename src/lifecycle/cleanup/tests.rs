//! Unit tests for the OpenCode guard of the closed-issue cleanup (issue
//! 747 P1).
//!
//! These drive the decision directly, without a Git or storage fixture:
//! the guard's only inputs are the mode, the candidate directory, the
//! sessions its persisted leases associate with it, and the answer the
//! host gives. The full-stack path (real lease rows, real worktree
//! guards, real `git worktree remove`) is covered by
//! `crate::worktree_tests::opencode_cleanup`.

use std::path::{Path, PathBuf};

use super::sessions::{SessionVerdict, evaluate};
use super::target::{MoveTarget, TargetResolver};
use super::{CleanupMode, associated_sessions};
use crate::worktree::LeaseRow;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;

fn row(worktree_path: &str, session: &str) -> LeaseRow {
    LeaseRow {
        lease_id: format!("lease-{session}"),
        repo_identity: "/repo/.git".to_owned(),
        issue: 7,
        session: session.to_owned(),
        checkout_path: "/repo".to_owned(),
        worktree_path: worktree_path.to_owned(),
        branch: "feat/7".to_owned(),
        status: crate::worktree::LEASE_STATUS_RETAINED.to_owned(),
        created_at: 0,
        heartbeat_at: 0,
        release_reason: None,
    }
}

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
) -> SessionVerdict {
    let ids: Vec<String> = associated.iter().map(|id| (*id).to_owned()).collect();
    evaluate(api, mode, target, &candidate(), &ids, closing)
}

/// A resolver that never touches Git: the fixtures name the target
/// directly so the decision table is exercised without a repository.
pub(super) fn fixed(target: MoveTarget) -> TargetResolver<'static> {
    TargetResolver::resolved(target)
}

// ---------------------------------------------------------------------------
// Association discovery
// ---------------------------------------------------------------------------

#[test]
fn associated_sessions_keeps_only_opencode_ids_pointing_at_the_directory() {
    let rows = vec![
        row("/tmp/pg-p1-candidate", "ses_a"),
        row("/tmp/pg-p1-candidate", "ses_b"),
        row("/tmp/pg-p1-candidate", "phasegent-cli"),
        row("/tmp/pg-p1-other", "ses_c"),
    ];
    assert_eq!(
        associated_sessions(&rows, &candidate()),
        vec!["ses_a".to_owned(), "ses_b".to_owned()]
    );
}

#[test]
fn a_candidate_without_an_opencode_lease_is_never_probed() {
    // A pure CLI close must keep its previous behaviour: no API traffic
    // at all, so an installed-but-unreachable OpenCode cannot change it.
    let api = FakeOpenCodeApi::empty().failing_session().failing_list();
    let verdict = evaluate_with(&api, CleanupMode::Close, &fixed(main_checkout()), &[], None);
    assert_eq!(verdict, SessionVerdict::Unmanaged);
    assert_eq!(api.list_calls(), 0);
    assert!(api.moves().is_empty());
}

// ---------------------------------------------------------------------------
// Close mode
// ---------------------------------------------------------------------------

#[test]
fn close_moves_the_own_session_and_confirms_with_fresh_evidence() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_main", Path::new("/tmp/pg-p1-main"));
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    assert_eq!(verdict, SessionVerdict::Vacant);
    assert_eq!(
        api.moves(),
        vec![("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned())]
    );
}

#[test]
fn a_queued_move_keeps_the_candidate() {
    // Acceptance is not relocation: the session still reports the
    // candidate, so the directory must survive this pass.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .queueing_moves();
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let SessionVerdict::Keep(reason) = verdict else {
        panic!("a queued move must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("has not left"), "got: {reason}");
}

#[test]
fn a_failed_move_request_keeps_the_candidate() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .failing_move();
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let SessionVerdict::Keep(reason) = verdict else {
        panic!("a failed move must keep the candidate, got {verdict:?}");
    };
    assert!(
        reason.contains("moving the closing OpenCode session failed"),
        "got: {reason}"
    );
}

#[test]
fn an_unverified_main_checkout_keeps_the_candidate_without_moving() {
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &candidate());
    let target = MoveTarget::Unavailable("bare repository".to_owned());
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(target),
        &["ses_a"],
        Some("ses_a"),
    );
    let SessionVerdict::Keep(reason) = verdict else {
        panic!("an unproven target must keep the candidate, got {verdict:?}");
    };
    assert!(
        reason.contains("no verified main checkout"),
        "got: {reason}"
    );
    assert!(
        api.moves().is_empty(),
        "nothing may be moved without a target"
    );
}

#[test]
fn close_never_moves_a_session_it_does_not_own() {
    // The closing session is known to the host and already sits at the
    // main checkout; the candidate hosts somebody else's session.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_other", &candidate())
        .with_session("ses_closer", Path::new("/tmp/pg-p1-main"));
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_other"],
        Some("ses_closer"),
    );
    let SessionVerdict::Keep(reason) = verdict else {
        panic!("another session must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("ses_other"), "got: {reason}");
    assert!(api.moves().is_empty());
    assert_eq!(api.list_calls(), 1, "the walk must stop before moving");
}

#[test]
fn close_never_moves_when_the_candidate_hosts_several_sessions() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_b", &candidate());
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a", "ses_b"],
        Some("ses_a"),
    );
    let SessionVerdict::Keep(reason) = verdict else {
        panic!("two occupants must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("2 OpenCode sessions"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn a_foreign_occupant_blocks_removal_without_being_moved() {
    // `ses_foreign` has no lease of this repository, so it is somebody
    // else's session and must never be relocated by this pass.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_foreign", &candidate());
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let SessionVerdict::Keep(reason) = verdict else {
        panic!("a foreign occupant must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("ses_foreign"), "got: {reason}");
    assert!(api.moves().is_empty());
}
