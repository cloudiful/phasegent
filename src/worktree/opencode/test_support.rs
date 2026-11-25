//! A scripted [`OpenCodeApi`] for tests.
//!
//! The retained cleanup tests drive the production decision against this fake,
//! so no test reaches a real OpenCode instance and no test can move a real
//! session. The fake models only the facts those cases use: where the host says
//! a session is, a session it does not own, a failing call and an unusable
//! listing.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use super::*;

/// One scripted session: where the host says it is, and which session it
/// descends from.
#[derive(Clone)]
struct FakeSession {
    directory: String,
    parent_id: Option<String>,
}

/// A stand-in host: one authoritative id → session map, served through
/// the same methods the production boundary implements.
pub(crate) struct FakeOpenCodeApi {
    reality: RefCell<BTreeMap<String, FakeSession>>,
    /// Ids whose `session.get` fails, to model an instance that does not
    /// own a session it was asked about.
    unknown: RefCell<Vec<String>>,
    fail_session: RefCell<bool>,
    fail_list: RefCell<bool>,
    /// A fixed `cursor.next` for every page, so a second page repeats a
    /// cursor the walk already followed and is rejected as incomplete.
    repeat_cursor: RefCell<Option<String>>,
    moves: RefCell<Vec<(String, String)>>,
    list_calls: RefCell<usize>,
    session_calls: RefCell<usize>,
    active_calls: RefCell<usize>,
    /// The ids the host reports as currently running.
    running: RefCell<Vec<String>>,
}

impl FakeOpenCodeApi {
    /// An empty host: no session exists anywhere and nothing is running.
    pub(crate) fn empty() -> Self {
        Self {
            reality: RefCell::new(BTreeMap::new()),
            unknown: RefCell::new(Vec::new()),
            fail_session: RefCell::new(false),
            fail_list: RefCell::new(false),
            repeat_cursor: RefCell::new(None),
            moves: RefCell::new(Vec::new()),
            list_calls: RefCell::new(0),
            session_calls: RefCell::new(0),
            active_calls: RefCell::new(0),
            running: RefCell::new(Vec::new()),
        }
    }

    /// A host that reports `id` as currently hosted in `directory`. This is a
    /// historical record: it does **not** make `id` running.
    pub(crate) fn with_session(self, id: &str, directory: &Path) -> Self {
        self.reality.borrow_mut().insert(
            id.to_owned(),
            FakeSession {
                directory: directory.to_string_lossy().into_owned(),
                parent_id: None,
            },
        );
        self
    }

    /// An id the host refuses to describe. `session.get` answering for it
    /// is exactly the standalone/private-instance uncertainty.
    pub(crate) fn with_unknown_session(self, id: &str) -> Self {
        self.unknown.borrow_mut().push(id.to_owned());
        self
    }

    /// A host that reports `id` as currently running and hosted in
    /// `directory`.
    pub(crate) fn with_running_session(self, id: &str, directory: &Path) -> Self {
        let session = self.with_session(id, directory);
        session.running.borrow_mut().push(id.to_owned());
        session
    }

    /// The parent `id` descends from. The lineage traversal reads the parent
    /// from the host, so a scripted parent makes that read observable.
    pub(crate) fn with_parent(self, id: &str, parent: &str) -> Self {
        if let Some(session) = self.reality.borrow_mut().get_mut(id) {
            session.parent_id = Some(parent.to_owned());
        }
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

    pub(crate) fn with_repeat_cursor(self, cursor: &str) -> Self {
        *self.repeat_cursor.borrow_mut() = Some(cursor.to_owned());
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

    /// How many single-session reads the host served.
    pub(crate) fn session_calls(&self) -> usize {
        *self.session_calls.borrow()
    }

    /// How many running-session queries the host served.
    pub(crate) fn active_calls(&self) -> usize {
        *self.active_calls.borrow()
    }
}

fn error(kind: &str) -> ApiError {
    ApiError::new(kind, &"scripted failure")
}

impl OpenCodeApi for FakeOpenCodeApi {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        *self.session_calls.borrow_mut() += 1;
        if *self.fail_session.borrow() {
            return Err(error("session.get"));
        }
        if self.unknown.borrow().iter().any(|known| known == id) {
            return Err(ApiError::session_not_found("session.get"));
        }
        let reality = self.reality.borrow();
        match reality.get(id) {
            Some(session) => Ok(LocatedSession {
                id: id.to_owned(),
                directory: session.directory.clone(),
                parent_id: session.parent_id.clone(),
            }),
            None => Err(ApiError::session_not_found("session.get")),
        }
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        *self.list_calls.borrow_mut() += 1;
        if *self.fail_list.borrow() {
            return Err(error("session.list"));
        }
        // An empty directory is the unfiltered listing: the host answers
        // with everything it knows and the caller does the comparing.
        let wanted = normalize(&request.directory);
        let reality = self.reality.borrow();
        let sessions: Vec<LocatedSession> = reality
            .iter()
            .filter(|(_, session)| wanted.is_empty() || normalize(&session.directory) == wanted)
            .map(|(id, session)| LocatedSession {
                id: id.clone(),
                directory: session.directory.clone(),
                parent_id: session.parent_id.clone(),
            })
            .collect();
        Ok(SessionPage {
            sessions,
            next_cursor: self.repeat_cursor.borrow().clone(),
        })
    }

    fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
        self.moves
            .borrow_mut()
            .push((id.to_owned(), directory.to_string_lossy().into_owned()));
        Ok(())
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        *self.active_calls.borrow_mut() += 1;
        Ok(self.running.borrow().clone())
    }
}

/// Compare paths lexically for the fake host: retained tests use literal
/// directory strings and never assert symlink or canonical equivalence.
fn normalize(directory: &str) -> String {
    directory.trim_end_matches('/').to_owned()
}
