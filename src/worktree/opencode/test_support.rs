//! A scripted [`OpenCodeApi`] for tests.
//!
//! Every P1 test drives the production cleanup against this fake, so no
//! test reaches a real OpenCode instance and no test can move a real
//! session. The fake models the one thing the cleanup depends on — where
//! the host says a session *is* — plus the ways an answer can fail to be
//! usable: a session the instance does not own, a failing call, a listing
//! that cannot be proven complete, and (issue 928 P1) a running-session set
//! that is unreadable, mismatched, or absent.
//!
//! A session recorded with [`FakeOpenCodeApi::with_session`] is a *historical
//! record* only: it is not implicitly running. A test that wants prune to see a
//! session as currently running must also call [`FakeOpenCodeApi::with_running`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use super::*;

/// One scripted session: where the host says it is.
#[derive(Clone)]
struct FakeSession {
    directory: String,
}

/// A stand-in host: one authoritative id → session map, served through
/// the same methods the production boundary implements.
pub(crate) struct FakeOpenCodeApi {
    reality: RefCell<BTreeMap<String, FakeSession>>,
    /// Ids whose `session.get` fails, to model an instance that does not
    /// own a session it was asked about.
    unknown: RefCell<Vec<String>>,
    /// Ids the listing never returns even though `session.get` describes
    /// them, to model a host whose own directory scoping under-reports.
    /// The authoritative per-session answer and the listing then disagree,
    /// which is the disagreement the cleanup must survive.
    hidden_from_list: RefCell<Vec<String>>,
    /// Ids that answer `session.get` until a move is accepted and go
    /// quiet afterwards, the way a host can stop describing a session
    /// right after it was asked to mutate one.
    unreadable_after_move: RefCell<Vec<String>>,
    fail_session: RefCell<bool>,
    fail_list: RefCell<bool>,
    fail_move: RefCell<bool>,
    /// The running-session set the `session.active` query answers with. Empty
    /// (the default) is a valid "nothing is running" answer.
    active: RefCell<Vec<String>>,
    /// Ids the `session.active` query reports, but whose `session.get` answers
    /// with a different id — a mismatched active answer the guard must reject.
    mismatched: RefCell<Vec<String>>,
    fail_active: RefCell<bool>,
    /// A fixed `cursor.next` for every page, so a second page repeats a
    /// cursor the walk already followed and is rejected as incomplete.
    repeat_cursor: RefCell<Option<String>>,
    /// Accept a move without relocating the session, the way a queued
    /// delivery leaves the session where it is.
    queue_moves: RefCell<bool>,
    moves: RefCell<Vec<(String, String)>>,
    list_calls: RefCell<usize>,
    /// The `directory` each listing call asked for, so a test can pin
    /// that the walk is unfiltered instead of trusting a host-side scope.
    list_directories: RefCell<Vec<String>>,
}

impl FakeOpenCodeApi {
    /// An empty host: no session exists anywhere and nothing is running.
    pub(crate) fn empty() -> Self {
        Self {
            reality: RefCell::new(BTreeMap::new()),
            unknown: RefCell::new(Vec::new()),
            hidden_from_list: RefCell::new(Vec::new()),
            unreadable_after_move: RefCell::new(Vec::new()),
            fail_session: RefCell::new(false),
            fail_list: RefCell::new(false),
            fail_move: RefCell::new(false),
            active: RefCell::new(Vec::new()),
            mismatched: RefCell::new(Vec::new()),
            fail_active: RefCell::new(false),
            repeat_cursor: RefCell::new(None),
            queue_moves: RefCell::new(false),
            moves: RefCell::new(Vec::new()),
            list_calls: RefCell::new(0),
            list_directories: RefCell::new(Vec::new()),
        }
    }

    /// A host that reports `id` as currently hosted in `directory`. This is a
    /// historical record: it does **not** make `id` running.
    pub(crate) fn with_session(self, id: &str, directory: &Path) -> Self {
        self.reality.borrow_mut().insert(
            id.to_owned(),
            FakeSession {
                directory: directory.to_string_lossy().into_owned(),
            },
        );
        self
    }

    /// Report `id` as currently running through the `session.active` query, so a
    /// guard that reads running state sees it as an occupant. A test wires it
    /// together with [`FakeOpenCodeApi::with_session`] to give it a location.
    pub(crate) fn with_running(self, id: &str) -> Self {
        self.active.borrow_mut().push(id.to_owned());
        self
    }

    /// Report `id` as running through `session.active` but have its
    /// `session.get` answer with a different id, modelling a mismatched answer
    /// the guard must reject.
    pub(crate) fn with_mismatched_running_id(self, id: &str) -> Self {
        self.active.borrow_mut().push(id.to_owned());
        self.mismatched.borrow_mut().push(id.to_owned());
        self
    }

    /// An id the host refuses to describe. `session.get` answering for it
    /// is exactly the standalone/private-instance uncertainty.
    pub(crate) fn with_unknown_session(self, id: &str) -> Self {
        self.unknown.borrow_mut().push(id.to_owned());
        self
    }

    /// A host that reports `id` as currently hosted in `directory` but
    /// never lists it, the way an instance whose own directory scoping
    /// under-reports still answers an authoritative per-session lookup.
    pub(crate) fn hidden_from_list(self, id: &str) -> Self {
        self.hidden_from_list.borrow_mut().push(id.to_owned());
        self
    }

    /// A host that describes `id` until the first accepted move request
    /// and refuses it afterwards.
    pub(crate) fn unreadable_after_move(self, id: &str) -> Self {
        self.unreadable_after_move.borrow_mut().push(id.to_owned());
        self
    }

    pub(crate) fn failing_session(self) -> Self {
        *self.fail_session.borrow_mut() = true;
        self
    }

    pub(crate) fn failing_list(self) -> Self {
        *self.fail_list.borrow_mut() = true;
        self
    }

    pub(crate) fn failing_move(self) -> Self {
        *self.fail_move.borrow_mut() = true;
        self
    }

    /// Make the `session.active` query fail, modelling an unavailable host.
    pub(crate) fn failing_active(self) -> Self {
        *self.fail_active.borrow_mut() = true;
        self
    }

    pub(crate) fn with_repeat_cursor(self, cursor: &str) -> Self {
        *self.repeat_cursor.borrow_mut() = Some(cursor.to_owned());
        self
    }

    /// Accept move requests without applying them.
    pub(crate) fn queueing_moves(self) -> Self {
        *self.queue_moves.borrow_mut() = true;
        self
    }

    /// `(session id, requested directory)` for every accepted move.
    pub(crate) fn moves(&self) -> Vec<(String, String)> {
        self.moves.borrow().clone()
    }

    /// How many listing pages the walk actually requested.
    pub(crate) fn list_calls(&self) -> usize {
        *self.list_calls.borrow()
    }

    /// The `directory` value of every listing call, in order.
    pub(crate) fn list_directories(&self) -> Vec<String> {
        self.list_directories.borrow().clone()
    }
}

fn error(kind: &str) -> ApiError {
    ApiError::new(kind, &"scripted failure")
}

impl OpenCodeApi for FakeOpenCodeApi {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        if *self.fail_session.borrow() {
            return Err(error("session.get"));
        }
        if !self.moves.borrow().is_empty()
            && self
                .unreadable_after_move
                .borrow()
                .iter()
                .any(|hidden| hidden == id)
        {
            return Err(error("session.get"));
        }
        if self.unknown.borrow().iter().any(|known| known == id) {
            return Err(ApiError::plain("SessionNotFoundError"));
        }
        let reality = self.reality.borrow();
        match reality.get(id) {
            Some(session) => {
                let reported_id = if self.mismatched.borrow().iter().any(|alias| alias == id) {
                    format!("{id}-other")
                } else {
                    id.to_owned()
                };
                Ok(LocatedSession {
                    id: reported_id,
                    directory: session.directory.clone(),
                })
            }
            None => Err(ApiError::plain("SessionNotFoundError")),
        }
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        *self.list_calls.borrow_mut() += 1;
        self.list_directories
            .borrow_mut()
            .push(request.directory.clone());
        if *self.fail_list.borrow() {
            return Err(error("session.list"));
        }
        // An empty directory is the unfiltered listing: the host answers
        // with everything it knows and the caller does the comparing.
        let wanted = normalize(&request.directory);
        let hidden = self.hidden_from_list.borrow();
        let reality = self.reality.borrow();
        let sessions: Vec<LocatedSession> = reality
            .iter()
            .filter(|(id, _)| !hidden.iter().any(|skipped| skipped == *id))
            .filter(|(_, session)| wanted.is_empty() || normalize(&session.directory) == wanted)
            .map(|(id, session)| LocatedSession {
                id: id.clone(),
                directory: session.directory.clone(),
            })
            .collect();
        Ok(SessionPage {
            sessions,
            next_cursor: self.repeat_cursor.borrow().clone(),
        })
    }

    fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
        if *self.fail_move.borrow() {
            return Err(error("session.move"));
        }
        self.moves
            .borrow_mut()
            .push((id.to_owned(), directory.to_string_lossy().into_owned()));
        if !*self.queue_moves.borrow() {
            let mut reality = self.reality.borrow_mut();
            let moved_to = directory.to_string_lossy().into_owned();
            match reality.get_mut(id) {
                Some(session) => session.directory = moved_to,
                None => {
                    reality.insert(
                        id.to_owned(),
                        FakeSession {
                            directory: moved_to,
                        },
                    );
                }
            }
        }
        Ok(())
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        if *self.fail_active.borrow() {
            return Err(error("session.active"));
        }
        let mut ids = self.active.borrow().clone();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

/// Reduce a path the way the production comparison does, so a lease row's
/// spelling and the host's spelling describe one directory.
fn normalize(directory: &str) -> String {
    let path = Path::new(directory);
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
