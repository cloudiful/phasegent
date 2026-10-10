//! Reconciliation-pass and move-target regression coverage for the
//! OpenCode-aware closed-issue cleanup (issue 747 P1).
//!
//! `issue sync` may look but never move, and `--no-clean` may not even
//! start the conversation, so both directions are pinned here together
//! with the resolution of the one directory a move is allowed to name.

use super::opencode_fixture::{Fixture, classified};
use super::support::*;
use super::*;

use crate::lifecycle::cleanup::target::{MoveTarget, resolve_main_checkout};
use crate::lifecycle::{AutoCleanupOutcome, CleanupMode};
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::{RawOutputRunner, parse_worktree_list};

// ---------------------------------------------------------------------------
// Sync mode
// ---------------------------------------------------------------------------

#[test]
fn sync_removes_a_candidate_the_close_already_vacated() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-sync-clean") else {
        return;
    };
    fixture.lease("ses_a");
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &fixture.main);
    assert_eq!(
        fixture.clean(&api, CleanupMode::Sync, None),
        AutoCleanupOutcome::Cleaned {
            removed: 1,
            kept: Vec::new()
        }
    );
    assert!(api.moves().is_empty(), "sync must never move a session");
    assert!(!fixture.candidate.exists());
}

#[test]
fn sync_keeps_a_candidate_whose_session_is_still_hosted() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-sync-keep") else {
        return;
    };
    fixture.lease("ses_a");
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &fixture.candidate);
    let (removed, kept) = classified(fixture.clean(&api, CleanupMode::Sync, None));
    assert_eq!(removed, 0);
    assert!(kept[0].contains("still hosted here"), "got: {}", kept[0]);
    assert!(api.moves().is_empty(), "sync must never move a session");
    assert!(fixture.candidate.exists(), "a later pass retries");
}

// ---------------------------------------------------------------------------
// Report mode
// ---------------------------------------------------------------------------

#[test]
fn report_mode_keeps_an_associated_candidate_without_calling_the_host() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-report") else {
        return;
    };
    fixture.lease("ses_a");
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &fixture.main);
    let (removed, kept) = classified(fixture.clean(&api, CleanupMode::Report, Some("ses_a")));
    assert_eq!(removed, 0);
    assert!(
        kept[0].contains("not probed in report mode"),
        "got: {}",
        kept[0]
    );
    assert_eq!(api.list_calls(), 0);
    assert!(fixture.candidate.exists());
}

#[test]
fn report_mode_still_classifies_a_pure_cli_candidate_with_the_local_guards() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-report-cli") else {
        return;
    };
    fixture.lease("phasegent-cli");
    let api = FakeOpenCodeApi::empty();
    assert_eq!(
        fixture.clean(&api, CleanupMode::Report, None),
        AutoCleanupOutcome::Cleaned {
            removed: 1,
            kept: Vec::new()
        },
        "the local guards alone decide an unassociated candidate"
    );
    assert_eq!(api.list_calls(), 0);
}

// ---------------------------------------------------------------------------
// Move target
// ---------------------------------------------------------------------------

#[test]
fn a_real_repository_resolves_its_main_checkout_through_the_porcelain_listing() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-target") else {
        return;
    };
    let target = resolve_main_checkout(&RawOutputRunner, &fixture.candidate, &fixture.identity);
    assert_eq!(
        target
            .resolve()
            .expect("a linked worktree yields a main checkout"),
        fixture.main.as_path()
    );
}

#[test]
fn a_bare_repository_is_never_a_move_target() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("p1-bare");
    let runner = ProcessWorktreeRunner::new();
    if runner
        .run(&["init", "-q", "--bare"], dir.path())
        .map(|output| output.status != 0)
        .unwrap_or(true)
    {
        return;
    }
    let identity = repo_identity(&runner, dir.path()).expect("bare identity");
    // A bare repository has no checkout to relocate a session into, so
    // the target must stay unproven instead of naming the bare directory.
    assert_eq!(
        resolve_main_checkout(&RawOutputRunner, dir.path(), &identity),
        MoveTarget::Unavailable("no main checkout could be verified".to_owned())
    );
}

#[test]
fn a_porcelain_listing_survives_the_raw_runner_but_not_the_sanitising_one() {
    // Guards the reason the raw runner exists: the production runner
    // strips newlines and truncates, which would silently drop every
    // worktree after the first.
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-raw") else {
        return;
    };
    let raw = RawOutputRunner
        .run(&["worktree", "list", "--porcelain"], &fixture.main)
        .expect("raw listing");
    let entries = parse_worktree_list(&raw.stdout);
    assert!(
        entries.len() >= 2,
        "the listing must carry the main checkout and the linked worktree: {raw:?}"
    );
    let sanitized = ProcessWorktreeRunner::new()
        .run(&["worktree", "list", "--porcelain"], &fixture.main)
        .expect("sanitised listing");
    assert!(
        !sanitized.stdout.contains('\n'),
        "the sanitising runner is why the raw one exists"
    );
}
