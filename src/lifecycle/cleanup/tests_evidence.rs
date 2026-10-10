//! Evidence-consolidation coverage for the OpenCode guard (issue 747 P3).
//!
//! Split from the decision table in [`super::tests`] because every case
//! here is about *disagreeing evidence* rather than about a single clean
//! answer: a host whose listing under-reports a session it will still
//! describe on request, a directory whose only occupant is the closing
//! session, a close argument that names a session but no lease, and the
//! re-read that stands between a requested move and a removed directory.
//!
//! The shared property is that uncertainty costs a directory. None of
//! these cases may end in a removal the host never agreed to.

use super::CleanupMode;
use super::sessions::SessionVerdict;
use super::tests::{candidate, evaluate_with, fixed, main_checkout};
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use std::path::Path;

// ---------------------------------------------------------------------------
// A listing that under-reports
// ---------------------------------------------------------------------------

#[test]
fn a_listing_that_omits_a_hosted_session_never_reads_as_vacant() {
    // The host answers `session.get` for a session its own listing drops.
    // The authoritative location is kept, so the directory is an occupied
    // one that this pass may look at, not an empty one it may delete.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .hidden_from_list("ses_a");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Sync,
        &fixed(main_checkout()),
        &["ses_a"],
        None,
    );
    let SessionVerdict::Keep(reason) = verdict else {
        panic!("an omitted occupant must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("still hosted here"), "got: {reason}");
    assert!(api.moves().is_empty());
}

#[test]
fn an_omitted_own_session_is_returned_rather_than_assumed_free() {
    // The same omission in the close chain: the directory is not empty,
    // so the closing session is moved out of it and only then is the
    // directory treated as free.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .hidden_from_list("ses_a");
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
        vec![("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned())],
        "an omission must cost a move request, never a silent removal"
    );
}

#[test]
fn the_occupancy_walk_asks_for_no_directory_filter() {
    // Enumeration is the last line of defence before a deletion, so it
    // never lets the host decide which rows this crate gets to see. An
    // empty value means "everything"; a server-side directory filter is
    // the thing under suspicion.
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &candidate());
    evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    assert!(
        !api.list_directories().is_empty(),
        "the guard must enumerate at least once"
    );
    assert!(
        api.list_directories().iter().all(|asked| asked.is_empty()),
        "every page must be requested without a directory filter, got {:?}",
        api.list_directories()
    );
}

// ---------------------------------------------------------------------------
// A close argument is not an association
// ---------------------------------------------------------------------------

#[test]
fn a_closing_opencode_argument_does_not_manage_a_pure_cli_candidate() {
    // `--worktree-session ses_x` names who may move a session. It says
    // nothing about this directory, so a candidate whose leases are all
    // plain CLI ids stays unmanaged and never reaches the API.
    let api = FakeOpenCodeApi::empty()
        .failing_session()
        .failing_list()
        .with_session("ses_x", &candidate());
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["phasegent-cli"],
        Some("ses_x"),
    );
    assert_eq!(verdict, SessionVerdict::Unmanaged);
    assert_eq!(api.list_calls(), 0, "an unmanaged candidate is not probed");
    assert!(api.moves().is_empty());
}

#[test]
fn a_candidate_with_no_lease_at_all_is_unmanaged_even_with_a_closing_session() {
    let api = FakeOpenCodeApi::empty().with_session("ses_x", &candidate());
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &[],
        Some("ses_x"),
    );
    assert_eq!(verdict, SessionVerdict::Unmanaged);
    assert_eq!(api.list_calls(), 0);
    assert!(api.moves().is_empty());
}

// ---------------------------------------------------------------------------
// Re-reading before a deletion
// ---------------------------------------------------------------------------

#[test]
fn an_omitted_second_session_blocks_the_move_before_any_request() {
    // The listing never showed `ses_b`, so a listing-only decision would
    // see one occupant and move. The kept direct answer makes it two, and
    // a directory with two occupants is not one this pass may empty.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_b", &candidate())
        .hidden_from_list("ses_b");
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
    assert!(
        api.moves().is_empty(),
        "an unseen occupant must be discovered before anything moves"
    );
}

#[test]
fn a_session_the_host_stops_describing_after_the_move_keeps_the_directory() {
    // The move is accepted and the closing session is confirmed at the
    // main checkout, then the host goes quiet about another associated
    // session. Only the post-move re-read can catch that, and it must:
    // uncertainty about a live session costs a directory, not a session.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .with_session("ses_b", Path::new("/tmp/pg-p1-main"))
        .unreadable_after_move("ses_b");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a", "ses_b"],
        Some("ses_a"),
    );
    let SessionVerdict::Keep(reason) = verdict else {
        panic!("an unconfirmable session must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("ses_b"), "got: {reason}");
    assert!(reason.contains("could not be located"), "got: {reason}");
    assert_eq!(
        api.moves(),
        vec![("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned())],
        "the re-read must run after the move, not before it"
    );
}
