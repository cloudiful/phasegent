//! Uncertainty and per-mode behaviour of the OpenCode guard
//! (issue 747 P1).
//!
//! Split from the close-mode table so each file stays readable: every
//! case here is a reason the guard must *keep* a directory, or a mode
//! boundary that decides who may act on the host's answer.

use super::CleanupMode;
use super::sessions::{FamilyOutcome, finalize_family};
use super::target::{MoveTarget, resolve_main_checkout};
use super::tests::{candidate, evaluate_with, fixed, main_checkout};
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};
use std::path::Path;

/// A host whose per-session answer reports authoritative absence while its
/// current listing still names the same session in the directory.
struct ContradictoryAbsenceHost {
    inner: FakeOpenCodeApi,
    id: String,
    directory: String,
}

impl ContradictoryAbsenceHost {
    fn new(inner: FakeOpenCodeApi, id: &str, directory: &Path) -> Self {
        Self {
            inner,
            id: id.to_owned(),
            directory: directory.to_string_lossy().into_owned(),
        }
    }
}

impl OpenCodeApi for ContradictoryAbsenceHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        let mut page = self.inner.list(request)?;
        page.sessions.push(LocatedSession {
            id: self.id.clone(),
            directory: self.directory.clone(),
            parent_id: None,
        });
        Ok(page)
    }

    fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.inner.active_sessions()
    }
}

// ---------------------------------------------------------------------------
// Uncertainty
// ---------------------------------------------------------------------------

#[test]
fn an_authoritatively_absent_idle_root_does_not_veto_removal() {
    let api = FakeOpenCodeApi::empty().with_unknown_session("ses_a");
    let verdict = evaluate_with(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &["ses_a"],
        Some("ses_a"),
    );
    assert_eq!(
        verdict,
        FamilyOutcome::Vacant,
        "an exact SessionNotFound with no contradicting occupant is absence, not uncertainty"
    );
    assert!(api.moves().is_empty());
}

#[test]
fn a_listing_that_contradicts_typed_absence_keeps_the_candidate() {
    let api = ContradictoryAbsenceHost::new(
        FakeOpenCodeApi::empty().with_unknown_session("ses_a"),
        "ses_a",
        Path::new("/tmp/pg-p1-candidate"),
    );
    let verdict = finalize_family(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned()],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a contradictory listing must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("reported as non-existent"), "got: {reason}");
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
    // A directory with no `ses_` association is decided by the local guards
    // in the close chain, but prune must still look at it: an occupant no
    // association names is somebody else's session and keeps the directory.
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

#[test]
fn close_and_sync_enumerate_an_unmanaged_directory_occupied_by_a_foreign_session() {
    for mode in [CleanupMode::Close, CleanupMode::Sync] {
        let api = FakeOpenCodeApi::empty().with_session("ses_foreign", &candidate());
        let verdict = finalize_family(&api, mode, &fixed(main_checkout()), &candidate(), &[], None);
        let FamilyOutcome::Keep(reason) = verdict else {
            panic!("{mode:?} must keep a foreign occupant, got {verdict:?}");
        };
        assert!(
            reason.contains("not associated") || reason.contains("occupies"),
            "got: {reason}"
        );
    }
}

#[test]
fn a_pure_cli_candidate_without_a_host_occupant_is_vacant_on_every_destructive_mode() {
    for mode in [
        CleanupMode::Close,
        CleanupMode::Sync,
        CleanupMode::Prune,
        CleanupMode::Verify,
    ] {
        let api = FakeOpenCodeApi::empty();
        assert_eq!(
            finalize_family(&api, mode, &fixed(main_checkout()), &candidate(), &[], None),
            FamilyOutcome::Vacant,
            "{mode:?}"
        );
    }
}

#[test]
fn sync_keeps_a_missing_root_whose_id_is_still_active() {
    let api = FakeOpenCodeApi::empty()
        .with_unknown_session("ses_a")
        .with_running("ses_a");
    let verdict = finalize_family(
        &api,
        CleanupMode::Sync,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned()],
        None,
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("an active id contradicting typed absence must keep, got {verdict:?}");
    };
    assert!(reason.contains("could not be located"), "got: {reason}");
}

#[test]
fn a_missing_root_listed_in_another_directory_still_contradicts_absence() {
    let api = ContradictoryAbsenceHost::new(
        FakeOpenCodeApi::empty().with_unknown_session("ses_a"),
        "ses_a",
        Path::new("/tmp/pg-p1-elsewhere"),
    );
    let verdict = finalize_family(
        &api,
        CleanupMode::Close,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned()],
        Some("ses_a"),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("the complete listing must contradict typed absence, got {verdict:?}");
    };
    assert!(reason.contains("reported as non-existent"), "got: {reason}");
    assert!(api.inner.moves().is_empty());
}
