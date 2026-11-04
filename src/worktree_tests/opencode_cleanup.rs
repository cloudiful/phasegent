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

/// A directory another issue is associated with belongs to that issue too,
/// so the conservative closure seam keeps it.
#[test]
fn a_directory_shared_with_another_issue_is_kept() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-shared") else {
        return;
    };
    fixture.lease("ses_a");
    fixture.lease_for_issue(8, "ses_b");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &fixture.main)
        .with_session("ses_b", &fixture.main);
    let outcome = fixture.clean(&api, CleanupMode::Close, Some("ses_a"));
    let (removed, kept) = classified(outcome);
    assert_eq!(removed, 0, "a shared directory must survive");
    assert_eq!(kept.len(), 1);
    assert!(
        kept[0].contains("issue(s) 8"),
        "the reason must name the other issue, got: {}",
        kept[0]
    );
    assert!(fixture.candidate.exists());
}

/// The scope-aware seam confirms the other issues through the configured
/// provider, so a directory whose other issue is confirmed closed is
/// removable.
#[test]
fn a_directory_whose_other_issue_is_closed_is_removed() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-shared-closed") else {
        return;
    };
    fixture.lease("ses_a");
    fixture.lease_for_issue(8, "ses_b");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &fixture.main)
        .with_session("ses_b", &fixture.main);
    let outcome = fixture.clean_with(&api, CleanupMode::Close, Some("ses_a"), &ConfirmAllClosed);
    let (removed, kept) = classified(outcome);
    assert_eq!(removed, 1, "a closed neighbour no longer protects it");
    assert!(kept.is_empty());
    assert!(!fixture.candidate.exists());
}

/// A closure seam that confirms every other issue, for the tests that are
/// about the guards and not about the provider.
struct ConfirmAllClosed;

impl crate::lifecycle::cleanup::closure::DirectoryIssueClosure for ConfirmAllClosed {
    fn confirm_others_closed(&self, _current_issue: u64, _issues: &[u64]) -> Result<(), String> {
        Ok(())
    }
}

/// The close's lease read is complete: an open issue whose only association
/// sits beyond a row ceiling still protects the shared directory.
#[test]
fn close_sees_an_open_issue_beyond_the_row_ceiling() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-beyond-open") else {
        return;
    };
    fixture.lease("ses_a");
    // 300 absent-history rows push the neighbour below the capped query's
    // ceiling; the close's read must still see it.
    for index in 0..300 {
        let storage = crate::infra::storage::Storage::open().expect("temp storage opens");
        let lease_id = format!("lease-hist-{index:03}");
        crate::worktree::leases::insert_lease(
            &storage,
            crate::worktree::leases::NewLease {
                lease_id: lease_id.as_str(),
                identity: &fixture.identity,
                issue: 700,
                session: "session-A",
                checkout_path: &fixture.main.to_string_lossy(),
                worktree_path: &format!("/tmp/phasegent-p1-history-{index:03}"),
                branch: "branch",
                status: crate::worktree::LEASE_STATUS_RELEASED,
                created_at: 1,
                heartbeat_at: 1,
            },
        )
        .expect("history rows insert");
    }
    fixture.lease_for_issue(8, "ses_b");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &fixture.main)
        .with_session("ses_b", &fixture.main);
    let outcome = fixture.clean(&api, CleanupMode::Close, Some("ses_a"));
    let (removed, kept) = classified(outcome);
    assert_eq!(removed, 0, "a beyond-ceiling open neighbour must protect");
    assert_eq!(kept.len(), 1);
    assert!(kept[0].contains("issue(s) 8"), "got: {}", kept[0]);
    assert!(fixture.candidate.exists());
    assert!(api.moves().is_empty());
}

/// The same completeness for the local guard: an *active* peer lease beyond
/// the ceiling keeps the directory with the active-lease reason.
#[test]
fn close_sees_an_active_peer_beyond_the_row_ceiling() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-beyond-active") else {
        return;
    };
    fixture.lease("ses_a");
    for index in 0..300 {
        let storage = crate::infra::storage::Storage::open().expect("temp storage opens");
        let lease_id = format!("lease-hist-{index:03}");
        crate::worktree::leases::insert_lease(
            &storage,
            crate::worktree::leases::NewLease {
                lease_id: lease_id.as_str(),
                identity: &fixture.identity,
                issue: 700,
                session: "session-A",
                checkout_path: &fixture.main.to_string_lossy(),
                worktree_path: &format!("/tmp/phasegent-p1-history-{index:03}"),
                branch: "branch",
                status: crate::worktree::LEASE_STATUS_RELEASED,
                created_at: 1,
                heartbeat_at: 1,
            },
        )
        .expect("history rows insert");
    }
    fixture.lease_for_issue_at(8, "ses_b", crate::worktree::LEASE_STATUS_ACTIVE);
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &fixture.main);
    let outcome = fixture.clean(&api, CleanupMode::Close, Some("ses_a"));
    let (removed, kept) = classified(outcome);
    assert_eq!(removed, 0, "a beyond-ceiling active peer must protect");
    assert_eq!(kept.len(), 1);
    assert!(kept[0].contains("active lease"), "got: {}", kept[0]);
    assert!(fixture.candidate.exists());
}

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

/// A host that injects lease-row state the moment it accepts the first move
/// request, modelling a peer that acquires the candidate while the family
/// is being relocated. The pre-move snapshot must not authorize the removal.
struct RaceOnMove<'a> {
    inner: FakeOpenCodeApi,
    storage: &'a crate::infra::storage::Storage,
    identity: &'a str,
    worktree_path: String,
    session: &'a str,
    /// The active peer's issue, when the race injects an active lease.
    active: bool,
    armed: std::cell::Cell<bool>,
}

impl RaceOnMove<'_> {
    fn inject(&self) {
        if self.armed.replace(false) {
            let status = if self.active {
                crate::worktree::LEASE_STATUS_ACTIVE
            } else {
                crate::worktree::LEASE_STATUS_RETAINED
            };
            crate::worktree::leases::insert_lease(
                self.storage,
                crate::worktree::leases::NewLease {
                    lease_id: "lease-incoming",
                    identity: self.identity,
                    issue: 8,
                    session: self.session,
                    checkout_path: "/fixture",
                    worktree_path: &self.worktree_path,
                    branch: "branch",
                    status,
                    created_at: 2,
                    heartbeat_at: 2,
                },
            )
            .expect("race row inserts");
        }
    }
}

impl crate::worktree::opencode::OpenCodeApi for RaceOnMove<'_> {
    fn session(
        &self,
        id: &str,
    ) -> Result<crate::worktree::opencode::LocatedSession, crate::worktree::opencode::ApiError>
    {
        self.inner.session(id)
    }
    fn list(
        &self,
        request: &crate::worktree::opencode::ListRequest,
    ) -> Result<crate::worktree::opencode::SessionPage, crate::worktree::opencode::ApiError> {
        self.inner.list(request)
    }
    fn move_session(
        &self,
        id: &str,
        directory: &Path,
    ) -> Result<(), crate::worktree::opencode::ApiError> {
        let accepted = self.inner.move_session(id, directory);
        if accepted.is_ok() {
            self.inject();
        }
        accepted
    }
    fn active_sessions(&self) -> Result<Vec<String>, crate::worktree::opencode::ApiError> {
        self.inner.active_sessions()
    }
}

/// A closure seam that confirms every other issue until the first refresh,
/// then reports the injected issue 8 as open. The pre-move answer may say
/// closed; the post-move recheck must not reuse it.
struct RaceClosure {
    refreshed: std::cell::Cell<bool>,
}

impl crate::lifecycle::cleanup::closure::DirectoryIssueClosure for RaceClosure {
    fn confirm_others_closed(&self, current_issue: u64, issues: &[u64]) -> Result<(), String> {
        for issue in issues {
            if *issue == current_issue {
                continue;
            }
            if *issue == 8 && self.refreshed.get() {
                return Err("issue 8 reopened after the moves".to_owned());
            }
        }
        Ok(())
    }
    fn refresh(&self) {
        self.refreshed.set(true);
    }
}

/// A new *active* lease acquired during the moves keeps the directory: the
/// pre-move snapshot alone never authorizes the removal.
#[test]
fn a_new_active_owner_during_the_moves_keeps_the_directory() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-race-active") else {
        return;
    };
    fixture.lease("ses_a");
    fixture.lease_for_issue_at(8, "ses_b", crate::worktree::LEASE_STATUS_RETAINED);
    let storage = crate::infra::storage::Storage::open().expect("temp storage opens");
    let api = RaceOnMove {
        inner: FakeOpenCodeApi::empty()
            .with_session("ses_a", &fixture.candidate)
            .with_session("ses_b", &fixture.main),
        storage: &storage,
        identity: &fixture.identity,
        worktree_path: fixture.candidate.to_string_lossy().into_owned(),
        session: "ses_new",
        active: true,
        armed: std::cell::Cell::new(true),
    };
    let outcome =
        fixture.clean_with_api(&api, CleanupMode::Close, Some("ses_a"), &ConfirmAllClosed);
    let (removed, kept) = classified(outcome);
    assert_eq!(removed, 0, "a new active owner during the moves keeps it");
    assert_eq!(kept.len(), 1);
    assert!(kept[0].contains("active lease"), "got: {}", kept[0]);
    assert!(fixture.candidate.exists());
}

#[test]
fn a_previously_unassociated_open_issue_during_close_keeps_the_directory() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-race-new-open") else {
        return;
    };
    fixture.lease("ses_a");
    let storage = crate::infra::storage::Storage::open().expect("temp storage opens");
    let api = RaceOnMove {
        inner: FakeOpenCodeApi::empty().with_session("ses_a", &fixture.candidate),
        storage: &storage,
        identity: &fixture.identity,
        worktree_path: fixture.candidate.to_string_lossy().into_owned(),
        session: "ses_new",
        active: false,
        armed: std::cell::Cell::new(true),
    };
    let closure = RaceClosure {
        refreshed: std::cell::Cell::new(false),
    };
    let outcome = fixture.clean_with_api(&api, CleanupMode::Close, Some("ses_a"), &closure);
    let (removed, kept) = classified(outcome);
    assert_eq!(removed, 0);
    assert_eq!(kept.len(), 1);
    assert!(
        kept[0].contains("issue closures changed"),
        "got: {}",
        kept[0]
    );
    assert!(closure.refreshed.get());
    assert!(!api.inner.moves().is_empty());
    assert!(fixture.candidate.exists());
}

/// A newly linked open issue that appears during the moves keeps the
/// directory, and the closure memo is refreshed rather than reused.
#[test]
fn a_new_open_issue_during_the_moves_keeps_the_directory() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = Fixture::new("p1-race-open") else {
        return;
    };
    fixture.lease("ses_a");
    fixture.lease_for_issue_at(8, "ses_b", crate::worktree::LEASE_STATUS_RETAINED);
    let storage = crate::infra::storage::Storage::open().expect("temp storage opens");
    let api = RaceOnMove {
        inner: FakeOpenCodeApi::empty()
            .with_session("ses_a", &fixture.candidate)
            .with_session("ses_b", &fixture.main),
        storage: &storage,
        identity: &fixture.identity,
        worktree_path: fixture.candidate.to_string_lossy().into_owned(),
        session: "ses_new",
        active: false,
        armed: std::cell::Cell::new(true),
    };
    let closure = RaceClosure {
        refreshed: std::cell::Cell::new(false),
    };
    let outcome = fixture.clean_with_api(&api, CleanupMode::Close, Some("ses_a"), &closure);
    let (removed, kept) = classified(outcome);
    assert_eq!(removed, 0, "a new open neighbour during the moves keeps it");
    assert_eq!(kept.len(), 1);
    assert!(
        kept[0].contains("issue closures changed"),
        "the post-move closure recheck must decide, got: {}",
        kept[0]
    );
    assert!(
        closure.refreshed.get(),
        "the closure memo must be refreshed after the moves"
    );
    assert!(fixture.candidate.exists());
}
