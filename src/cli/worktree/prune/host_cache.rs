//! Invocation-scoped read cache over the OpenCode host API for the prune
//! *scan* only (issues 917 P1, 928 P1).
//!
//! A global prune visits many lease rows that resolve to the same directory and
//! many repositories that repeat the same running-session questions. The scan
//! phase is read-only and conservative: a repeated question may safely reuse its
//! first answer within one invocation, while a *removal* must never be
//! authorized by a cached answer (see `worktrees::prune_pass`, which always
//! re-reads through the original API before deleting).
//!
//! Two outcomes are memoized, both with their failures:
//!
//! * `session.active` — the running-session id set;
//! * `session.get` per session id — the location of a running session.
//!
//! Neither a historical listing nor a move is reachable through this view: the
//! scan never enumerates history and never moves a session, so `list` and
//! `move_session` are denied explicitly rather than forwarded. The cache is a
//! pure read layer and is intentionally not `Send` — one cache lives inside one
//! invocation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

/// A read-only memo over another [`OpenCodeApi`].
pub(crate) struct ScanHostCache<'a> {
    inner: &'a dyn OpenCodeApi,
    active: RefCell<Option<Result<Vec<String>, ApiError>>>,
    sessions: RefCell<HashMap<String, Result<LocatedSession, ApiError>>>,
}

impl<'a> ScanHostCache<'a> {
    pub(crate) fn new(inner: &'a dyn OpenCodeApi) -> Self {
        Self {
            inner,
            active: RefCell::new(None),
            sessions: RefCell::new(HashMap::new()),
        }
    }
}

impl OpenCodeApi for ScanHostCache<'_> {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        if let Some(answer) = self.sessions.borrow().get(id) {
            return answer.clone();
        }
        let answer = self.inner.session(id);
        self.sessions
            .borrow_mut()
            .insert(id.to_owned(), answer.clone());
        answer
    }

    fn list(&self, _request: &ListRequest) -> Result<SessionPage, ApiError> {
        Err(ApiError::plain(
            "the prune scan reads only the running-session set; it never enumerates history",
        ))
    }

    fn move_session(&self, _id: &str, _directory: &Path) -> Result<(), ApiError> {
        Err(ApiError::plain(
            "the prune scan cache is read-only and never moves a session",
        ))
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        if let Some(answer) = self.active.borrow().as_ref() {
            return answer.clone();
        }
        let answer = self.inner.active_sessions();
        *self.active.borrow_mut() = Some(answer.clone());
        answer
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;

    use super::ScanHostCache;
    use crate::worktree::opencode::test_support::FakeOpenCodeApi;
    use crate::worktree::opencode::{
        ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage,
    };

    /// A host that counts the questions actually forwarded to it, so a test can
    /// prove the cache answered a repeat locally.
    struct CountingHost {
        inner: FakeOpenCodeApi,
        sessions: Cell<usize>,
        active: Cell<usize>,
    }

    impl CountingHost {
        fn new(inner: FakeOpenCodeApi) -> Self {
            Self {
                inner,
                sessions: Cell::new(0),
                active: Cell::new(0),
            }
        }
    }

    impl OpenCodeApi for CountingHost {
        fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
            self.sessions.set(self.sessions.get() + 1);
            self.inner.session(id)
        }

        fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
            self.inner.list(request)
        }

        fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
            self.inner.move_session(id, directory)
        }

        fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
            self.active.set(self.active.get() + 1);
            self.inner.active_sessions()
        }
    }

    #[test]
    fn memoizes_active_and_session_outcomes() {
        let host = CountingHost::new(
            FakeOpenCodeApi::empty()
                .with_session("ses_a", Path::new("/x"))
                .with_running("ses_a"),
        );
        let cache = ScanHostCache::new(&host);
        // Two identical questions of each kind; the cache answers the second.
        assert_eq!(cache.active_sessions().unwrap(), vec!["ses_a".to_owned()]);
        assert_eq!(cache.active_sessions().unwrap(), vec!["ses_a".to_owned()]);
        assert!(cache.session("ses_a").is_ok());
        assert!(cache.session("ses_a").is_ok());
        assert_eq!(host.active.get(), 1, "the active query is memoized");
        assert_eq!(host.sessions.get(), 1, "the session lookup is memoized");
    }

    #[test]
    fn refuses_to_enumerate_history_or_move_a_session() {
        let host = FakeOpenCodeApi::empty();
        let cache = ScanHostCache::new(&host);
        assert!(
            cache
                .list(&ListRequest {
                    directory: String::new(),
                    cursor: None,
                })
                .is_err(),
            "the scan must never enumerate historical sessions"
        );
        assert!(
            cache.move_session("ses_a", Path::new("/x")).is_err(),
            "the scan cache must never move a session"
        );
        assert!(host.moves().is_empty());
        assert_eq!(host.list_calls(), 0);
    }
}
