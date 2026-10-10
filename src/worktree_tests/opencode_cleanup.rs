//! Close-mode regression coverage for the OpenCode-aware closed-issue
//! cleanup (issue 747 P1).
//!
//! The close chain is the only pass that may relocate a session, so this
//! suite pins the whole move arm end to end against a real repository:
//! the local guards, the host's answer, the move request and its
//! confirmation.

use super::opencode_fixture::{Fixture, classified};
use super::*;

use crate::lifecycle::{AutoCleanupOutcome, CleanupMode};
use crate::worktree::opencode::is_opencode_session;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;

#[test]
fn close_removes_a_candidate_whose_session_is_already_at_the_main_checkout() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-confirmed") else {
        return;
    };
    fixture.lease("ses_a");
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &fixture.main);
    let outcome = fixture.clean(&api, CleanupMode::Close, Some("ses_a"));
    assert_eq!(
        outcome,
        AutoCleanupOutcome::Cleaned {
            removed: 1,
            kept: Vec::new()
        }
    );
    assert!(!fixture.candidate.exists(), "the candidate must be gone");
    assert!(api.moves().is_empty(), "nothing left to move");
}

#[test]
fn close_moves_the_own_session_out_and_then_removes_the_candidate() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-move") else {
        return;
    };
    fixture.lease("ses_a");
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &fixture.candidate);
    let outcome = fixture.clean(&api, CleanupMode::Close, Some("ses_a"));
    assert_eq!(
        outcome,
        AutoCleanupOutcome::Cleaned {
            removed: 1,
            kept: Vec::new()
        }
    );
    assert_eq!(
        api.moves(),
        vec![(
            "ses_a".to_owned(),
            fixture.main.to_string_lossy().into_owned()
        )],
        "the move target must be the verified main checkout"
    );
    assert!(!fixture.candidate.exists());
}

#[test]
fn a_queued_move_keeps_the_directory_for_a_later_pass() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-queued") else {
        return;
    };
    fixture.lease("ses_a");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &fixture.candidate)
        .queueing_moves();
    let (removed, kept) = classified(fixture.clean(&api, CleanupMode::Close, Some("ses_a")));
    assert_eq!(removed, 0);
    assert_eq!(kept.len(), 1);
    assert!(kept[0].contains("has not left"), "got: {}", kept[0]);
    assert!(fixture.candidate.exists(), "the candidate must survive");
}

#[test]
fn an_unreachable_host_keeps_the_directory_and_warns() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-apifail") else {
        return;
    };
    fixture.lease("ses_a");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &fixture.candidate)
        .failing_session();
    let (removed, kept) = classified(fixture.clean(&api, CleanupMode::Close, Some("ses_a")));
    assert_eq!(removed, 0);
    assert!(kept[0].contains("could not be located"), "got: {}", kept[0]);
    assert!(fixture.candidate.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn a_session_the_instance_does_not_own_keeps_the_directory() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-unknown") else {
        return;
    };
    fixture.lease("ses_a");
    let api = FakeOpenCodeApi::empty().with_unknown_session("ses_a");
    let (removed, kept) = classified(fixture.clean(&api, CleanupMode::Close, Some("ses_a")));
    assert_eq!(removed, 0);
    assert!(kept[0].contains("could not be located"), "got: {}", kept[0]);
    assert!(fixture.candidate.exists());
}

#[test]
fn a_foreign_occupant_keeps_the_directory_without_being_moved() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-foreign") else {
        return;
    };
    fixture.lease("ses_a");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &fixture.main)
        .with_session("ses_stranger", &fixture.candidate);
    let (removed, kept) = classified(fixture.clean(&api, CleanupMode::Close, Some("ses_a")));
    assert_eq!(removed, 0);
    assert!(kept[0].contains("ses_stranger"), "got: {}", kept[0]);
    assert!(
        api.moves().is_empty(),
        "a stranger's session is never moved"
    );
    assert!(fixture.candidate.exists());
}

#[test]
fn a_listing_that_omits_the_hosted_session_costs_a_move_not_a_silent_removal() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p3-omitted") else {
        return;
    };
    fixture.lease("ses_a");
    // The host describes the session but never lists it: a listing-only
    // decision would see an empty directory and delete it. The kept
    // direct answer turns the same facts into a move of our own session.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &fixture.candidate)
        .hidden_from_list("ses_a");
    let (removed, kept) = classified(fixture.clean(&api, CleanupMode::Close, Some("ses_a")));
    assert_eq!(removed, 1);
    assert!(kept.is_empty(), "got: {kept:?}");
    assert_eq!(
        api.moves(),
        vec![(
            "ses_a".to_owned(),
            fixture.main.to_string_lossy().into_owned()
        )],
        "an omitted occupant must be returned before the directory goes"
    );
}

#[test]
fn a_pure_cli_candidate_is_never_managed_by_its_closing_argument() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p3-cli-arg") else {
        return;
    };
    fixture.lease("phasegent-cli");
    // The close names an OpenCode session, but no lease associates one
    // with this directory, so the candidate keeps its pre-existing
    // behaviour and the host is never contacted.
    let api = FakeOpenCodeApi::empty()
        .failing_session()
        .failing_list()
        .with_session("ses_a", &fixture.candidate);
    assert_eq!(
        fixture.clean(&api, CleanupMode::Close, Some("ses_a")),
        AutoCleanupOutcome::Cleaned {
            removed: 1,
            kept: Vec::new()
        }
    );
    assert_eq!(api.list_calls(), 0);
    assert!(api.moves().is_empty());
}

// ---------------------------------------------------------------------------
// Pure CLI and the local guards
// ---------------------------------------------------------------------------

#[test]
fn a_pure_cli_candidate_is_cleaned_without_any_api_traffic() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-cli") else {
        return;
    };
    fixture.lease("phasegent-cli");
    assert!(!is_opencode_session("phasegent-cli"));
    let api = FakeOpenCodeApi::empty().failing_session().failing_list();
    assert_eq!(
        fixture.clean(&api, CleanupMode::Close, None),
        AutoCleanupOutcome::Cleaned {
            removed: 1,
            kept: Vec::new()
        }
    );
    assert_eq!(api.list_calls(), 0);
    assert!(api.moves().is_empty());
}

#[test]
fn a_dirty_candidate_is_kept_by_the_local_guard_before_the_host_is_asked() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-dirty") else {
        return;
    };
    fixture.lease("ses_a");
    std::fs::write(fixture.candidate.join("scratch.txt"), "uncommitted").expect("write");
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &fixture.main);
    let (removed, kept) = classified(fixture.clean(&api, CleanupMode::Close, Some("ses_a")));
    assert_eq!(removed, 0);
    assert!(
        kept[0].contains("uncommitted or untracked files"),
        "got: {}",
        kept[0]
    );
    assert!(api.moves().is_empty());
}

#[test]
fn the_main_checkout_is_never_a_candidate() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-main") else {
        return;
    };
    // Point the lease at the main checkout itself: the guard must keep it
    // whatever the host says.
    let main = fixture.main.clone();
    fixture.lease_at(&main, "ses_a");
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &main);
    let (removed, kept) = classified(fixture.clean(&api, CleanupMode::Close, Some("ses_a")));
    assert_eq!(removed, 0);
    assert!(
        kept[0].contains("main checkout is never removed"),
        "got: {}",
        kept[0]
    );
    assert!(main.exists());
}
