//! Uncertainty and per-mode behaviour of the OpenCode guard
//! (issue 747 P1).
//!
//! Split from the close-mode table so each file stays readable: every
//! case here is a reason the guard must *keep* a directory, or a mode
//! boundary that decides who may act on the host's answer.

use super::CleanupMode;
use super::sessions::FamilyOutcome;
use super::target::{MoveTarget, resolve_main_checkout};
use super::tests::{candidate, evaluate_with, fixed, main_checkout};
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use std::path::Path;

// ---------------------------------------------------------------------------
// Uncertainty
// ---------------------------------------------------------------------------

#[test]
fn a_session_the_instance_does_not_own_is_uncertainty_not_absence() {
    let api = FakeOpenCodeApi::empty().with_unknown_session("ses_a");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a missing session must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("could not be located"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn a_failing_listing_keeps_the_candidate() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", Path::new("/tmp/pg-p1-main"))
        .failing_list();
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an unusable listing must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("incomplete"), "got: {reason}");
}

#[test]
fn a_repeated_cursor_keeps_the_candidate() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", Path::new("/tmp/pg-p1-main"))
        .with_repeat_cursor("cursor-1");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a non-terminating walk must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("incomplete"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn a_failing_session_lookup_keeps_the_candidate() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", Path::new("/tmp/pg-p1-main"))
        .failing_session();
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an unreadable host must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("could not be located"), "got: {reason}");
}

// ---------------------------------------------------------------------------
// Sync and report modes
// ---------------------------------------------------------------------------

#[test]
fn sync_never_moves_but_still_finds_a_vacated_candidate() {
    // After a close relocated the session, sync finds the candidate free.
    let api = FakeOpenCodeApi::empty().with_session("ses_a", Path::new("/tmp/pg-p1-main"));
    assert_eq!(
        evaluate_with(
            &api,
            CleanupMode::Sync,
            &fixed(main_checkout()),
            &["ses_a"],
            None
        ),
        FamilyOutcome::Vacant
    );
    assert!(api.moves().is_empty());
}

#[test]
fn sync_keeps_a_candidate_whose_session_is_still_hosted() {
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &candidate());
    let verdict = evaluate_with(
        &api,
        CleanupMode::Sync,
        &fixed(main_checkout()),
        &["ses_a"],
        None,
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("sync must not move a session, got {verdict:?}");
    };
    assert!(reason.contains("still hosted here"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn report_mode_keeps_an_associated_candidate_without_probing() {
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .failing_session()
        .failing_list();
    let verdict = evaluate_with(
        &api,
        CleanupMode::Report,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("report mode must keep an associated candidate, got {verdict:?}");
    };
    assert!(
        reason.contains("not probed in report mode"),
        "got: {reason}"
    );
    assert_eq!(api.list_calls(), 0, "report mode must not call the API");
    assert!(api.moves().is_empty());
}

#[test]
fn report_mode_leaves_an_unassociated_candidate_to_the_local_guards() {
    let api = FakeOpenCodeApi::empty();
    assert_eq!(
        evaluate_with(
            &api,
            CleanupMode::Report,
            &fixed(main_checkout()),
            &[],
            None
        ),
        FamilyOutcome::Unmanaged
    );
    assert_eq!(api.list_calls(), 0);
}

// ---------------------------------------------------------------------------
// Move-target resolution
// ---------------------------------------------------------------------------

#[test]
fn an_empty_identity_yields_no_target_rather_than_a_guessed_directory() {
    let target = resolve_main_checkout(
        &crate::worktree::ProcessWorktreeRunner::new(),
        Path::new("/nonexistent-phasegent-p1"),
        "",
    );
    assert_eq!(
        target,
        MoveTarget::Unavailable("the repository identity could not be resolved".to_owned())
    );
}

// ---------------------------------------------------------------------------
// The prune chain in the shared assessment
// ---------------------------------------------------------------------------

#[test]
fn prune_enumerates_an_unmanaged_candidate_so_a_foreign_occupant_is_seen() {
    // A directory with no `ses_` lease is decided by the local guards in the
    // close chain, but prune must still look at it: an occupant no lease
    // associates is somebody else's session and keeps the directory.
    let api = FakeOpenCodeApi::empty()
        .failing_session()
        .with_session("ses_foreign", &candidate());
    let verdict = evaluate_with(&api, CleanupMode::Prune, &fixed(main_checkout()), &[], None);
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a foreign occupant must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("ses_foreign"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn the_prune_removal_and_preview_share_the_failure_surface() {
    // An unusable running-session answer keeps the directory in both chains:
    // the preview reports it and the removal meets the same blocker.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .failing_active();
    for mode in [CleanupMode::Prune, CleanupMode::Preview] {
        let verdict = evaluate_with(&api, mode, &fixed(main_checkout()), &["ses_a"], None);
        let FamilyOutcome::Keep(reason) = verdict else {
            panic!("an unreadable running answer must keep the candidate, got {verdict:?}");
        };
        assert!(
            reason.contains("running-session query failed"),
            "mode {mode:?}: got: {reason}"
        );
    }
    assert!(api.moves().is_empty());
}
