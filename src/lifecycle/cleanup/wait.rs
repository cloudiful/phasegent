//! The injectable move-confirmation policy of the shared cleanup.
//!
//! An accepted `session.move` is not proof that the session relocated: the
//! host may defer delivery until the next prompt boundary. `issue close`,
//! `issue sync`, and the prune preview keep their historical no-wait
//! behaviour, while an actual prune may spend one invocation-shared short
//! budget confirming a pending move. The budget is policy about deliberate
//! polling, never a promise about the latency of an external command.
//!
//! The first read after an accepted move is always immediate. Only a readable
//! answer that still places the exact session in the old candidate enters the
//! wait loop; a destination is confirmed, and a third location, a mismatched
//! id, a non-absolute location, or an unreadable answer keeps immediately. The
//! loop never reissues the move and re-reads running state on every poll so a
//! session that starts running stops the wait.

use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::worktree::opencode::{LocatedSession, OpenCodeApi};
use crate::worktree::{bounded, same_directory};

/// The total scheduling budget one prune invocation may spend waiting for
/// pending moves, shared across every directory and session.
pub(crate) const MOVE_WAIT_BUDGET: Duration = Duration::from_secs(3);

/// The interval between two confirmation reads inside that budget.
pub(crate) const MOVE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Injectable time so a test can spend the whole budget without sleeping.
pub(crate) trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// One invocation's accumulated waiting budget. The deadline is fixed by the
/// first pending accepted move and never reset per directory or session.
pub(crate) struct MoveBudget {
    clock: Box<dyn Clock>,
    deadline: RefCell<Option<Instant>>,
}

impl MoveBudget {
    pub(crate) fn new() -> Self {
        Self::with_clock(Box::new(SystemClock))
    }

    pub(crate) fn with_clock(clock: Box<dyn Clock>) -> Self {
        Self {
            clock,
            deadline: RefCell::new(None),
        }
    }

    fn deadline(&self) -> Instant {
        let clock = &self.clock;
        *self
            .deadline
            .borrow_mut()
            .get_or_insert_with(|| clock.now() + MOVE_WAIT_BUDGET)
    }

    fn expired(&self) -> bool {
        self.clock.now() >= self.deadline()
    }

    fn remaining(&self) -> Duration {
        self.deadline().saturating_duration_since(self.clock.now())
    }

    fn poll(&self, duration: Duration) {
        self.clock.sleep(duration);
    }

    #[cfg(test)]
    pub(crate) fn deadline_for_test(&self) -> Instant {
        self.deadline()
    }
}

/// Which confirmation discipline one cleanup pass uses.
#[derive(Clone, Copy)]
pub(crate) enum MovePolicy<'a> {
    /// Historical behaviour: one immediate read, no waiting.
    NoWait,
    /// Actual prune: one immediate read plus the shared bounded wait.
    Bounded(&'a MoveBudget),
}

/// Where the host currently places an exact session, reduced to the three
/// answers the confirmation distinguishes.
enum Placement {
    Destination,
    Old,
    Third(String),
    WrongId(String),
    NonAbsolute(String),
}

impl<'a> MovePolicy<'a> {
    /// Confirm one accepted move. `Ok(())` means the exact session is at the
    /// destination; `Err(reason)` is the bounded keep reason.
    pub(crate) fn confirm(
        &self,
        api: &dyn OpenCodeApi,
        id: &str,
        old: &Path,
        destination: &Path,
        mut running_check: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        match self.observe(api, id, old, destination)? {
            Placement::Destination => Ok(()),
            Placement::Old => match self {
                Self::NoWait => Err(not_left(id)),
                Self::Bounded(budget) => loop {
                    if budget.expired() {
                        return Err(wait_expired(id));
                    }
                    running_check()?;
                    // The host check itself may consume the remaining budget;
                    // never schedule a poll or a new observation past it.
                    if budget.expired() {
                        return Err(wait_expired(id));
                    }
                    let remaining = budget.remaining();
                    if remaining.is_zero() {
                        return Err(wait_expired(id));
                    }
                    budget.poll(remaining.min(MOVE_POLL_INTERVAL));
                    if budget.expired() {
                        return Err(wait_expired(id));
                    }
                    match self.observe(api, id, old, destination)? {
                        Placement::Destination => return Ok(()),
                        Placement::Old => {}
                        other => return Err(self.keep_reason(id, other)),
                    }
                },
            },
            other => Err(self.keep_reason(id, other)),
        }
    }

    fn keep_reason(&self, id: &str, placement: Placement) -> String {
        match placement {
            Placement::WrongId(other) => format!(
                "the host answered for the moved OpenCode session '{id}' with a different session '{}'",
                bounded(&other)
            ),
            Placement::NonAbsolute(location) => format!(
                "the host answered for the moved OpenCode session '{id}' with a non-absolute location '{}'",
                bounded(&location)
            ),
            Placement::Third(location) => match self {
                Self::NoWait => not_left(id),
                Self::Bounded(_) => format!(
                    "OpenCode session '{id}' is now in '{}', not the directory it was to be returned from",
                    bounded(&location)
                ),
            },
            Placement::Destination | Placement::Old => String::new(),
        }
    }

    fn observe(
        &self,
        api: &dyn OpenCodeApi,
        id: &str,
        old: &Path,
        destination: &Path,
    ) -> Result<Placement, String> {
        let located = match api.session(id) {
            Ok(located) => located,
            Err(error) => {
                return Err(format!(
                    "the OpenCode session '{id}' could not be confirmed after the move request: {}",
                    bounded(&error.message)
                ));
            }
        };
        Ok(placement(located, id, old, destination))
    }
}

fn placement(located: LocatedSession, id: &str, old: &Path, destination: &Path) -> Placement {
    if located.id != id {
        return Placement::WrongId(located.id);
    }
    if !Path::new(&located.directory).is_absolute() {
        return Placement::NonAbsolute(located.directory);
    }
    let reported = Path::new(&located.directory);
    if same_directory(reported, destination) {
        Placement::Destination
    } else if same_directory(reported, old) {
        Placement::Old
    } else {
        Placement::Third(located.directory)
    }
}

fn not_left(id: &str) -> String {
    format!("the OpenCode session '{id}' has not left the directory yet")
}

fn wait_expired(id: &str) -> String {
    format!("{} (bounded wait expired)", not_left(id))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use super::{Clock, MOVE_WAIT_BUDGET, MoveBudget};

    struct FakeClock {
        now: RefCell<Instant>,
        sleeps: Cell<usize>,
    }

    impl Clock for Rc<FakeClock> {
        fn now(&self) -> Instant {
            *self.now.borrow()
        }

        fn sleep(&self, duration: Duration) {
            *self.now.borrow_mut() += duration;
            self.sleeps.set(self.sleeps.get() + 1);
        }
    }

    #[test]
    fn one_budget_deadline_is_shared_and_never_reset() {
        let clock = Rc::new(FakeClock {
            now: RefCell::new(Instant::now()),
            sleeps: Cell::new(0),
        });
        let budget = MoveBudget::with_clock(Box::new(clock.clone()));
        let first = budget.deadline_for_test();
        assert_eq!(first, budget.deadline_for_test());
        assert_eq!(first.duration_since(clock.now()), MOVE_WAIT_BUDGET);
        clock.sleep(Duration::from_secs(1));
        assert_eq!(
            budget.deadline_for_test(),
            first,
            "a later directory or session must not reset the deadline"
        );
    }
}
