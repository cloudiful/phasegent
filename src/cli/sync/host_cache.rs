//! Invocation-scoped OpenCode read memo for the reconciliation pass
//! (issue 1054 P6).
//!
//! A sync pass asks the same host questions while it walks many candidate
//! directories: the unfiltered per-directory occupancy listing is identical
//! for every directory, and the same session is looked up once per candidate
//! that references it. The answers are read-only at scan time, so a repeated
//! *successful* answer may be served from one invocation-local memo.
//!
//! The memo is never deletion evidence by itself. If a directory would be
//! removed on an answer that used cached data, the pass repeats the exact
//! production occupancy predicate through the direct, uncached API before it
//! removes anything (see [`SyncOccupancyGuard`]). A cached "keep" is
//! conservative and is not refreshed. Failures are never memoized, and the
//! memo refuses every mutating call, so a reconciliation pass can never move
//! a session through this layer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;

use crate::lifecycle::cleanup::candidate::{self, OccupancyGuard};
use crate::lifecycle::cleanup::{CleanupMode, TargetResolver};
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

/// A read-through memo over another [`OpenCodeApi`].
pub(crate) struct SyncHostCache<'a> {
    inner: &'a dyn OpenCodeApi,
    sessions: RefCell<HashMap<String, LocatedSession>>,
    pages: RefCell<HashMap<ListKey, SessionPage>>,
    /// Whether any read in the current per-directory window was served
    /// from the memo instead of the host.
    hit: Cell<bool>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ListKey {
    directory: String,
    cursor: Option<String>,
}

impl<'a> SyncHostCache<'a> {
    pub(crate) fn new(inner: &'a dyn OpenCodeApi) -> Self {
        Self {
            inner,
            sessions: RefCell::new(HashMap::new()),
            pages: RefCell::new(HashMap::new()),
            hit: Cell::new(false),
        }
    }

    /// Open a fresh hit window for one candidate directory. Every read the
    /// directory's occupancy check performs reports into this window, so a
    /// free verdict knows whether it relied on memoized data.
    pub(crate) fn begin_directory(&self) {
        self.hit.set(false);
    }

    /// True when any read in the current window was served from the memo.
    pub(crate) fn directory_used_cache(&self) -> bool {
        self.hit.get()
    }
}

impl OpenCodeApi for SyncHostCache<'_> {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        if let Some(answer) = self.sessions.borrow().get(id) {
            self.hit.set(true);
            return Ok(answer.clone());
        }
        let answer = self.inner.session(id)?;
        self.sessions
            .borrow_mut()
            .insert(id.to_owned(), answer.clone());
        Ok(answer)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        let key = ListKey {
            directory: request.directory.clone(),
            cursor: request.cursor.clone(),
        };
        if let Some(answer) = self.pages.borrow().get(&key) {
            self.hit.set(true);
            return Ok(answer.clone());
        }
        let answer = self.inner.list(request)?;
        self.pages.borrow_mut().insert(key, answer.clone());
        Ok(answer)
    }

    fn move_session(&self, _id: &str, _directory: &Path) -> Result<(), ApiError> {
        Err(ApiError::plain(
            "the reconciliation pass never moves an OpenCode session",
        ))
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.inner.active_sessions()
    }
}

/// The optimization-aware host guard used only by the sync pass.
///
/// It answers from the memo, but a would-be-free verdict that used memoized
/// data is re-evaluated through the same [`candidate::occupancy_reason`]
/// predicate with the direct API before the directory may be removed. A
/// directory whose reads were all fresh is never queried twice, and a keep
/// verdict is always conservative.
pub(crate) struct SyncOccupancyGuard<'a> {
    cache: &'a SyncHostCache<'a>,
    direct: &'a dyn OpenCodeApi,
}

impl<'a> SyncOccupancyGuard<'a> {
    pub(crate) fn new(cache: &'a SyncHostCache<'a>, direct: &'a dyn OpenCodeApi) -> Self {
        Self { cache, direct }
    }
}

impl OccupancyGuard for SyncOccupancyGuard<'_> {
    fn keep_reason(
        &self,
        mode: CleanupMode,
        target: &TargetResolver<'_>,
        directory: &Path,
        associated: &[String],
        closing: Option<&str>,
    ) -> Option<String> {
        if mode != CleanupMode::Sync {
            return candidate::occupancy_reason(
                self.direct,
                mode,
                target,
                directory,
                associated,
                closing,
            );
        }
        self.cache.begin_directory();
        let cached =
            candidate::occupancy_reason(self.cache, mode, target, directory, associated, closing);
        if cached.is_none() && self.cache.directory_used_cache() {
            return candidate::occupancy_reason(
                self.direct,
                mode,
                target,
                directory,
                associated,
                closing,
            );
        }
        cached
    }
}
