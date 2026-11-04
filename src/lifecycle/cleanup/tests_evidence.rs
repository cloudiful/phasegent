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
use super::sessions::FamilyOutcome;
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
    let FamilyOutcome::Keep(reason) = verdict else {
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
    assert_eq!(
        verdict,
        FamilyOutcome::Released {
            sessions: vec!["ses_a".to_owned()]
        }
    );
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
    assert_eq!(verdict, FamilyOutcome::Unmanaged);
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
    assert_eq!(verdict, FamilyOutcome::Unmanaged);
    assert_eq!(api.list_calls(), 0);
    assert!(api.moves().is_empty());
}

// ---------------------------------------------------------------------------
// Re-reading before a deletion
// ---------------------------------------------------------------------------

#[test]
fn an_omitted_second_session_is_returned_with_the_first() {
    // The listing never showed `ses_b`, so a listing-only decision would
    // move one session and declare the directory free. The kept direct
    // answer makes it two, and the family is returned as a whole: the
    // directory is only removed once every member of it has left.
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
    assert_eq!(
        verdict,
        FamilyOutcome::Released {
            sessions: vec!["ses_a".to_owned(), "ses_b".to_owned()]
        }
    );
    assert_eq!(
        api.moves(),
        vec![
            ("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned()),
            ("ses_b".to_owned(), "/tmp/pg-p1-main".to_owned()),
        ],
        "an unseen occupant must be discovered before anything is removed"
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
    let FamilyOutcome::Keep(reason) = verdict else {
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

/// A host that keeps reporting the old candidate for the first reads after the
/// move was accepted, then reports the destination: a delivery that lands on a
/// later boundary.
struct DelayedDeliveryHost {
    inner: FakeOpenCodeApi,
    id: String,
    accepted: std::cell::Cell<bool>,
    reads_since_move: std::cell::Cell<usize>,
    destination: String,
    delay_reads: usize,
}

impl crate::worktree::opencode::OpenCodeApi for DelayedDeliveryHost {
    fn session(
        &self,
        id: &str,
    ) -> Result<crate::worktree::opencode::LocatedSession, crate::worktree::opencode::ApiError>
    {
        if id == self.id && self.accepted.get() {
            let reads = self.reads_since_move.get();
            self.reads_since_move.set(reads + 1);
            if reads >= self.delay_reads {
                return Ok(crate::worktree::opencode::LocatedSession {
                    id: id.to_owned(),
                    directory: self.destination.clone(),
                    parent_id: None,
                });
            }
        }
        self.inner.session(id)
    }

    fn list(
        &self,
        request: &crate::worktree::opencode::ListRequest,
    ) -> Result<crate::worktree::opencode::SessionPage, crate::worktree::opencode::ApiError> {
        let mut page = self.inner.list(request)?;
        if self.accepted.get() && self.reads_since_move.get() >= self.delay_reads {
            page.sessions.retain(|session| session.id != self.id);
        }
        Ok(page)
    }

    fn move_session(
        &self,
        id: &str,
        directory: &Path,
    ) -> Result<(), crate::worktree::opencode::ApiError> {
        let result = self.inner.move_session(id, directory);
        if result.is_ok() && id == self.id {
            self.accepted.set(true);
        }
        result
    }

    fn active_sessions(&self) -> Result<Vec<String>, crate::worktree::opencode::ApiError> {
        self.inner.active_sessions()
    }
}

struct FakeClock {
    now: std::cell::RefCell<std::time::Instant>,
    sleeps: std::cell::Cell<usize>,
    durations: std::cell::RefCell<Vec<std::time::Duration>>,
}

impl FakeClock {
    fn new() -> std::rc::Rc<Self> {
        std::rc::Rc::new(Self {
            now: std::cell::RefCell::new(std::time::Instant::now()),
            sleeps: std::cell::Cell::new(0),
            durations: std::cell::RefCell::new(Vec::new()),
        })
    }
}

impl crate::lifecycle::cleanup::wait::Clock for std::rc::Rc<FakeClock> {
    fn now(&self) -> std::time::Instant {
        *self.now.borrow()
    }

    fn sleep(&self, duration: std::time::Duration) {
        *self.now.borrow_mut() += duration;
        self.sleeps.set(self.sleeps.get() + 1);
        self.durations.borrow_mut().push(duration);
    }
}

fn bounded_budget(clock: &std::rc::Rc<FakeClock>) -> crate::lifecycle::cleanup::wait::MoveBudget {
    crate::lifecycle::cleanup::wait::MoveBudget::with_clock(Box::new(clock.clone()))
}

#[test]
fn a_delayed_move_confirms_within_the_shared_budget_without_real_sleeping() {
    use crate::lifecycle::cleanup::wait::MovePolicy;
    let inner = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .queueing_moves();
    let api = DelayedDeliveryHost {
        inner,
        id: "ses_a".to_owned(),
        accepted: std::cell::Cell::new(false),
        reads_since_move: std::cell::Cell::new(0),
        destination: "/tmp/pg-p1-main".to_owned(),
        delay_reads: 4,
    };
    let clock = FakeClock::new();
    let budget = bounded_budget(&clock);
    let verdict = super::sessions::finalize_family_with(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned()],
        None,
        MovePolicy::Bounded(&budget),
    );
    assert_eq!(
        verdict,
        FamilyOutcome::Released {
            sessions: vec!["ses_a".to_owned()]
        }
    );
    assert_eq!(
        api.inner.moves(),
        vec![("ses_a".to_owned(), "/tmp/pg-p1-main".to_owned())],
        "the move is requested exactly once"
    );
    assert!(
        clock.sleeps.get() > 0 && clock.sleeps.get() <= 30,
        "the wait polls inside the 3-second budget, got {}",
        clock.sleeps.get()
    );
}

#[test]
fn an_unconfirmed_move_times_out_with_a_truthful_waiting_reason() {
    use crate::lifecycle::cleanup::wait::MovePolicy;
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .queueing_moves();
    let clock = FakeClock::new();
    let budget = bounded_budget(&clock);
    let verdict = super::sessions::finalize_family_with(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned()],
        None,
        MovePolicy::Bounded(&budget),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a never-delivered move must keep the candidate, got {verdict:?}");
    };
    assert!(
        reason.contains("has not left the directory yet"),
        "got: {reason}"
    );
    assert!(reason.contains("bounded wait expired"), "got: {reason}");
    assert_eq!(
        api.moves().len(),
        1,
        "waiting never reissues the move request"
    );
    assert_eq!(
        clock.sleeps.get(),
        30,
        "3 seconds of 100ms polls, with no real sleeping"
    );
}

/// A bounded wait keeps immediately when the accepted move lands somewhere
/// other than the candidate or the verified destination; it never reissues the
/// request and never polls.
#[test]
fn a_bounded_wait_keeps_a_third_location_immediately() {
    use crate::lifecycle::cleanup::wait::MovePolicy;
    struct ThirdHost {
        inner: FakeOpenCodeApi,
        id: String,
        accepted: std::cell::Cell<bool>,
    }
    impl crate::worktree::opencode::OpenCodeApi for ThirdHost {
        fn session(
            &self,
            id: &str,
        ) -> Result<crate::worktree::opencode::LocatedSession, crate::worktree::opencode::ApiError>
        {
            if id == self.id && self.accepted.get() {
                return Ok(crate::worktree::opencode::LocatedSession {
                    id: id.to_owned(),
                    directory: "/tmp/pg-p1-third".to_owned(),
                    parent_id: None,
                });
            }
            self.inner.session(id)
        }

        fn list(
            &self,
            request: &crate::worktree::opencode::ListRequest,
        ) -> Result<crate::worktree::opencode::SessionPage, crate::worktree::opencode::ApiError>
        {
            self.inner.list(request)
        }

        fn move_session(
            &self,
            id: &str,
            directory: &Path,
        ) -> Result<(), crate::worktree::opencode::ApiError> {
            let result = self.inner.move_session(id, directory);
            if result.is_ok() && id == self.id {
                self.accepted.set(true);
            }
            result
        }

        fn active_sessions(&self) -> Result<Vec<String>, crate::worktree::opencode::ApiError> {
            self.inner.active_sessions()
        }
    }
    let api = ThirdHost {
        inner: FakeOpenCodeApi::empty()
            .with_session("ses_a", &candidate())
            .queueing_moves(),
        id: "ses_a".to_owned(),
        accepted: std::cell::Cell::new(false),
    };
    let clock = FakeClock::new();
    let budget = bounded_budget(&clock);
    let verdict = super::sessions::finalize_family_with(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned()],
        None,
        MovePolicy::Bounded(&budget),
    );
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a third location must keep the candidate, got {verdict:?}");
    };
    assert!(
        reason.contains("not the directory it was to be returned from"),
        "got: {reason}"
    );
    assert_eq!(clock.sleeps.get(), 0, "a third path is never polled");
}

/// A host whose running check consumes fake time once a move was accepted.
struct SlowRunningHost {
    inner: FakeOpenCodeApi,
    id: String,
    accepted: std::cell::Cell<bool>,
    clock: std::rc::Rc<FakeClock>,
    check_duration: std::time::Duration,
}

impl crate::worktree::opencode::OpenCodeApi for SlowRunningHost {
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
        let result = self.inner.move_session(id, directory);
        if result.is_ok() && id == self.id {
            self.accepted.set(true);
        }
        result
    }

    fn active_sessions(&self) -> Result<Vec<String>, crate::worktree::opencode::ApiError> {
        if self.accepted.get() {
            use crate::lifecycle::cleanup::wait::Clock;
            self.clock.sleep(self.check_duration);
        }
        self.inner.active_sessions()
    }
}

fn run_bounded_slow_check(
    check_duration: std::time::Duration,
) -> (FamilyOutcome, std::rc::Rc<FakeClock>) {
    use crate::lifecycle::cleanup::wait::MovePolicy;
    let clock = FakeClock::new();
    let inner = FakeOpenCodeApi::empty()
        .with_session("ses_a", &candidate())
        .queueing_moves();
    let api = SlowRunningHost {
        inner,
        id: "ses_a".to_owned(),
        accepted: std::cell::Cell::new(false),
        clock: clock.clone(),
        check_duration,
    };
    let budget = bounded_budget(&clock);
    let verdict = super::sessions::finalize_family_with(
        &api,
        CleanupMode::Prune,
        &fixed(main_checkout()),
        &candidate(),
        &["ses_a".to_owned()],
        None,
        MovePolicy::Bounded(&budget),
    );
    (verdict, clock)
}

#[test]
fn a_running_check_that_consumes_the_budget_schedules_no_further_wait() {
    let (verdict, clock) = run_bounded_slow_check(std::time::Duration::from_secs(3));
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a consumed budget must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("bounded wait expired"), "got: {reason}");
    assert_eq!(
        clock.durations.borrow().as_slice(),
        &[std::time::Duration::from_secs(3)],
        "only the host check's own time is spent; no extra poll is scheduled"
    );
}

#[test]
fn a_partial_remainder_is_clamped_and_never_polls_past_the_deadline() {
    let (verdict, clock) = run_bounded_slow_check(std::time::Duration::from_millis(2950));
    let FamilyOutcome::Keep(reason) = verdict else {
        panic!("a consumed budget must keep the candidate, got {verdict:?}");
    };
    assert!(reason.contains("bounded wait expired"), "got: {reason}");
    assert_eq!(
        clock.durations.borrow().as_slice(),
        &[
            std::time::Duration::from_millis(2950),
            std::time::Duration::from_millis(50),
        ],
        "the poll is clamped to the remaining deadline"
    );
}
