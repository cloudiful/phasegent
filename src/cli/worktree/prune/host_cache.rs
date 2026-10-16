//! Invocation-scoped read cache over the OpenCode host API for the prune
//! *scan* only (issue 917 P1).
//!
//! A global prune visits many lease rows that resolve to the same directory and
//! many repositories that repeat the same occupancy questions. The scan phase
//! is read-only and conservative: a repeated question may safely reuse its
//! first answer within one invocation, while a *removal* must never be
//! authorized by a cached answer (see `worktrees::prune_pass`, which always
//! re-reads through the original API before deleting).
//!
//! Two outcomes are memoized, both with their failures:
//!
//! * `session.get` per session id — including a failure, so an unreachable host
//!   is not re-probed once per directory;
//! * `session.list` per `(directory, cursor)` request — the paginated outcome
//!   is cached page by page, keyed by the request that produced it, so a
//!   partial page is never mistaken for a complete snapshot and the pagination
//!   bounds and failure semantics of the underlying walk are unchanged.
//!
//! The cache is a pure read layer: [`OpenCodeApi::move_session`] is refused so
//! a scan can never move a session, and this type is intentionally not `Send`
//! — one cache lives inside one invocation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

/// One memoized `session.list` outcome, keyed by the request that produced it
/// (`(directory, cursor)`).
type PageKey = (String, Option<String>);
type PageOutcome = Result<SessionPage, ApiError>;

/// A read-only memo over another [`OpenCodeApi`].
pub(crate) struct ScanHostCache<'a> {
    inner: &'a dyn OpenCodeApi,
    sessions: RefCell<HashMap<String, Result<LocatedSession, ApiError>>>,
    pages: RefCell<HashMap<PageKey, PageOutcome>>,
}

impl<'a> ScanHostCache<'a> {
    pub(crate) fn new(inner: &'a dyn OpenCodeApi) -> Self {
        Self {
            inner,
            sessions: RefCell::new(HashMap::new()),
            pages: RefCell::new(HashMap::new()),
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

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        let key = (request.directory.clone(), request.cursor.clone());
        if let Some(answer) = self.pages.borrow().get(&key) {
            return answer.clone();
        }
        let answer = self.inner.list(request);
        self.pages.borrow_mut().insert(key, answer.clone());
        answer
    }

    fn move_session(&self, _id: &str, _directory: &Path) -> Result<(), ApiError> {
        Err(ApiError::plain(
            "the prune scan cache is read-only and never moves a session",
        ))
    }
}

/// A read-only projection over another [`OpenCodeApi`] that omits a set of
/// already-validated associated child sessions from every listing (issues 918
/// P2, 923 P1).
///
/// The shared candidate guard evaluates occupancy through this view so an
/// eligible associated child does not read as a foreign occupant. It hides only
/// the ids handed to it, never rewrites a directory, refuses
/// [`OpenCodeApi::move_session`], and is used for the *scan* only: the removal
/// path always re-proves vacancy through the original, uncached API.
pub(crate) struct ProjectedHost<'a> {
    inner: &'a dyn OpenCodeApi,
    omitted: std::collections::BTreeSet<String>,
}

impl<'a> ProjectedHost<'a> {
    pub(crate) fn new(inner: &'a dyn OpenCodeApi, omitted: &[String]) -> Self {
        Self {
            inner,
            omitted: omitted.iter().cloned().collect(),
        }
    }
}

impl OpenCodeApi for ProjectedHost<'_> {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        let mut page = self.inner.list(request)?;
        page.sessions
            .retain(|session| !self.omitted.contains(&session.id));
        Ok(page)
    }

    fn move_session(&self, _id: &str, _directory: &Path) -> Result<(), ApiError> {
        Err(ApiError::plain(
            "the prune projection is read-only and never moves a session",
        ))
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
        lists: Cell<usize>,
    }

    impl CountingHost {
        fn new(inner: FakeOpenCodeApi) -> Self {
            Self {
                inner,
                sessions: Cell::new(0),
                lists: Cell::new(0),
            }
        }
    }

    impl OpenCodeApi for CountingHost {
        fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
            self.sessions.set(self.sessions.get() + 1);
            self.inner.session(id)
        }

        fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
            self.lists.set(self.lists.get() + 1);
            self.inner.list(request)
        }

        fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
            self.inner.move_session(id, directory)
        }
    }

    fn page_request(directory: &str) -> ListRequest {
        ListRequest {
            directory: directory.to_owned(),
            cursor: None,
        }
    }

    #[test]
    fn memoizes_session_and_listing_outcomes() {
        let host =
            CountingHost::new(FakeOpenCodeApi::empty().with_session("ses_a", Path::new("/x")));
        let cache = ScanHostCache::new(&host);
        let request = page_request("/x");
        // Two identical questions of each kind; the cache answers the second.
        assert!(cache.session("ses_a").is_ok());
        assert!(cache.session("ses_a").is_ok());
        assert!(cache.list(&request).is_ok());
        assert!(cache.list(&request).is_ok());
        assert_eq!(host.sessions.get(), 1, "the session lookup is memoized");
        assert_eq!(host.lists.get(), 1, "the listing outcome is memoized");
    }

    #[test]
    fn memoizes_a_failed_listing_so_it_is_not_retried() {
        let host = CountingHost::new(
            FakeOpenCodeApi::empty()
                .with_session("ses_a", Path::new("/x"))
                .failing_list(),
        );
        let cache = ScanHostCache::new(&host);
        let request = page_request("/x");
        assert!(cache.list(&request).is_err());
        assert!(cache.list(&request).is_err());
        assert_eq!(
            host.lists.get(),
            1,
            "the failed listing is memoized, not repeated"
        );
    }

    #[test]
    fn refuses_to_move_a_session() {
        let host = FakeOpenCodeApi::empty();
        let cache = ScanHostCache::new(&host);
        assert!(
            cache.move_session("ses_a", Path::new("/x")).is_err(),
            "the scan cache must never move a session"
        );
        assert!(host.moves().is_empty());
    }
}
