use super::cli_support::*;
use super::support::*;
use super::*;

use crate::lifecycle::cleanup::TargetResolver;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

/// A host that answers `session.get` for `id` with a *different* session id,
/// the way a host can misreport which session it described. The answer still
/// carries the requested session's directory, so only the identity check can
/// catch it.
struct MismatchedIdHost {
    inner: FakeOpenCodeApi,
    id: String,
}

impl MismatchedIdHost {
    fn new(inner: FakeOpenCodeApi, id: &str) -> Self {
        Self {
            inner,
            id: id.to_owned(),
        }
    }
}

impl OpenCodeApi for MismatchedIdHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        let located = self.inner.session(id)?;
        if id == self.id {
            return Ok(LocatedSession {
                id: format!("{id}-other"),
                directory: located.directory,
            });
        }
        Ok(located)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.inner.list(request)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.inner.active_sessions()
    }
}

/// A host that starts reporting a session as running once the removal stage
/// has read the occupancy, so a session that began running after the scan is
/// caught by the relocation stage's own running check.
struct RunningAfterDiscoveryHost {
    inner: FakeOpenCodeApi,
    /// How many occupancy listings the host already answered.
    listings: std::cell::Cell<usize>,
}

impl RunningAfterDiscoveryHost {
    fn new(inner: FakeOpenCodeApi) -> Self {
        Self {
            inner,
            listings: std::cell::Cell::new(0),
        }
    }
}

impl OpenCodeApi for RunningAfterDiscoveryHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.listings.set(self.listings.get() + 1);
        self.inner.list(request)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        // The scan and the removal recheck see nothing running; the
        // relocation stage's own check sees the session running.
        if self.listings.get() == 0 {
            return Ok(Vec::new());
        }
        self.inner.active_sessions()
    }
}

/// A host that answers with a different session id for the first read after
/// the move request was accepted, so the move confirmation itself is
/// untrustworthy.
struct MismatchedAfterMoveHost {
    inner: FakeOpenCodeApi,
    id: String,
    /// Whether the host accepted a move request for `id`.
    moved: std::cell::Cell<bool>,
    /// How many reads the host answered since the move was accepted.
    reads_after_move: std::cell::Cell<usize>,
}

impl MismatchedAfterMoveHost {
    fn new(inner: FakeOpenCodeApi, id: &str) -> Self {
        Self {
            inner,
            id: id.to_owned(),
            moved: std::cell::Cell::new(false),
            reads_after_move: std::cell::Cell::new(0),
        }
    }
}

impl OpenCodeApi for MismatchedAfterMoveHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        let located = self.inner.session(id)?;
        if id == self.id && self.moved.get() {
            let reads = self.reads_after_move.get();
            self.reads_after_move.set(reads + 1);
            if reads == 0 {
                return Ok(LocatedSession {
                    id: format!("{id}-other"),
                    directory: located.directory,
                });
            }
        }
        Ok(located)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.inner.list(request)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        let result = self.inner.move_session(id, directory);
        if result.is_ok() && id == self.id {
            self.moved.set(true);
        }
        result
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.inner.active_sessions()
    }
}

/// A host that gains a new occupant after its first occupancy listing, the way
/// a session can appear while the associated sessions are being returned.
struct NewOccupantHost {
    inner: FakeOpenCodeApi,
    occupant: LocatedSession,
    /// How many occupancy listings the host already answered.
    listings: std::cell::Cell<usize>,
}

impl NewOccupantHost {
    fn new(inner: FakeOpenCodeApi, occupant: LocatedSession) -> Self {
        Self {
            inner,
            occupant,
            listings: std::cell::Cell::new(0),
        }
    }
}

impl OpenCodeApi for NewOccupantHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        let mut page = self.inner.list(request)?;
        let listings = self.listings.get();
        self.listings.set(listings + 1);
        // The first listing is the discovery and stays clean; the fresh
        // listing after the move finds the new occupant.
        if listings > 0 {
            page.sessions.push(self.occupant.clone());
        }
        Ok(page)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.inner.active_sessions()
    }
}

/// A host that answers with an unusable location for one read after the move
/// request was accepted, so a malformed answer reaches the confirmation or
/// the vacancy check.
struct RelativeAfterMoveHost {
    inner: FakeOpenCodeApi,
    id: String,
    /// Whether the host accepted a move request for `id`.
    moved: std::cell::Cell<bool>,
    /// How many reads the host answered since the move was accepted.
    reads_after_move: std::cell::Cell<usize>,
    /// Which read after the move answers with the unusable location.
    relative_at: usize,
}

impl RelativeAfterMoveHost {
    fn new(inner: FakeOpenCodeApi, id: &str, relative_at: usize) -> Self {
        Self {
            inner,
            id: id.to_owned(),
            moved: std::cell::Cell::new(false),
            reads_after_move: std::cell::Cell::new(0),
            relative_at,
        }
    }
}

impl OpenCodeApi for RelativeAfterMoveHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        let located = self.inner.session(id)?;
        if id == self.id && self.moved.get() {
            let reads = self.reads_after_move.get();
            self.reads_after_move.set(reads + 1);
            if reads == self.relative_at {
                return Ok(LocatedSession {
                    id: id.to_owned(),
                    directory: "relative/unknown".to_owned(),
                });
            }
        }
        Ok(located)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.inner.list(request)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        let result = self.inner.move_session(id, directory);
        if result.is_ok() && id == self.id {
            self.moved.set(true);
        }
        result
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.inner.active_sessions()
    }
}

/// A host wrapper that counts every read (`active_sessions`/`session`) it
/// forwards, so a test can prove a locally-excluded directory made no host call.
struct CountingHost {
    inner: FakeOpenCodeApi,
    reads: std::cell::Cell<usize>,
    lists: std::cell::Cell<usize>,
}

impl CountingHost {
    fn new(inner: FakeOpenCodeApi) -> Self {
        Self {
            inner,
            reads: std::cell::Cell::new(0),
            lists: std::cell::Cell::new(0),
        }
    }

    fn reads(&self) -> usize {
        self.reads.get()
    }

    /// How many occupancy listings the wrapper forwarded.
    fn lists(&self) -> usize {
        self.lists.get()
    }

    /// The moves the inner host accepted.
    fn moves(&self) -> Vec<(String, String)> {
        self.inner.moves()
    }
}

impl OpenCodeApi for CountingHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.reads.set(self.reads.get() + 1);
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.reads.set(self.reads.get() + 1);
        self.lists.set(self.lists.get() + 1);
        self.inner.list(request)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.reads.set(self.reads.get() + 1);
        self.inner.active_sessions()
    }
}

/// A host that implements only the close/sync surface, so the running-session
/// query falls back to the trait default and fails closed (issue 928 P1).
struct NoActiveHost {
    inner: FakeOpenCodeApi,
}

impl OpenCodeApi for NoActiveHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.inner.list(request)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }
}

/// A directory that looked removable at scan time but whose session is freshly
/// reported as *running* during the final recheck must be kept: the removal
/// authorization never trusts the scan snapshot.
#[test]
fn prune_final_recheck_uses_the_uncached_host_and_keeps_a_newly_running_path() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-final-fresh") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-final-fresh");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-fresh01");
    lease(
        &storage,
        "lease-fresh",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // The scan sees nothing running (vacant); the fresh host reports the session
    // as now running in the candidate.
    let scan_host = FakeOpenCodeApi::empty().with_session("ses_a", repo.dir.path());
    let fresh_host = FakeOpenCodeApi::empty()
        .with_session("ses_a", &target)
        .with_running("ses_a");
    let rows = list_all_for_repo(&storage, &identity).expect("list");
    let pinned = pinned_active_paths(&rows);
    let target_resolver = TargetResolver::new(repo.dir.path(), &identity);
    let scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        rows: &rows,
        target: &target_resolver,
    };
    let source = ScriptedIssueStateSource::closed();
    let summary = prune_pass(
        &storage,
        &runner,
        &scan_host,
        &fresh_host,
        &scope,
        &pinned,
        &source,
        999,
        PruneMode::Remove,
        &|_| {},
    );
    assert_eq!(
        summary.removed, 0,
        "a freshly running session keeps the directory"
    );
    assert!(target.exists());
    assert!(fresh_host.moves().is_empty(), "prune never moves a session");
    let row = list_all_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-fresh")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RETAINED);
}

/// Build a scope and pinned-active set over the temp database's rows and run
/// the pass, so the guards run against a real checkout.
fn run_pass(
    storage: &Storage,
    repo: &TempRepo,
    identity: &str,
    api: &dyn OpenCodeApi,
    source: &dyn IssueStateSource,
    mode: PruneMode,
) -> PruneSummary {
    let runner = ProcessWorktreeRunner::new();
    let rows = list_all_for_repo(storage, identity).expect("list");
    let pinned = pinned_active_paths(&rows);
    let target = TargetResolver::new(repo.dir.path(), identity);
    let scope = PruneScope {
        identity,
        repo_path: repo.dir.path(),
        rows: &rows,
        target: &target,
    };
    prune_pass(
        storage,
        &runner,
        api,
        api,
        &scope,
        &pinned,
        source,
        999,
        mode,
        &|_| {},
    )
}

/// Insert a lease with an explicit issue number and a fresh heartbeat.
fn lease_for_issue(
    storage: &Storage,
    lease_id: &str,
    identity: &str,
    session: &str,
    status: &str,
    worktree_path: &str,
    issue: u64,
) {
    let now = now_unix_secs();
    insert_lease(
        storage,
        NewLease {
            lease_id,
            identity,
            issue,
            session,
            checkout_path: "/tmp/checkout",
            worktree_path,
            branch: &format!("phasegent/{issue}-aaaaaa"),
            status,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("insert row");
}

fn lease(
    storage: &Storage,
    lease_id: &str,
    identity: &str,
    session: &str,
    status: &str,
    worktree_path: &str,
) {
    lease_for_issue(
        storage,
        lease_id,
        identity,
        session,
        status,
        worktree_path,
        7,
    );
}

#[test]
fn prune_keeps_the_main_checkout() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-main") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-main");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    lease(
        &storage,
        "lease-main",
        &identity,
        "ses_probe",
        LEASE_STATUS_RETAINED,
        &repo.dir.path().to_string_lossy(),
    );
    // A `ses_` lease whose host answer would probe the API if the local guard
    // did not run first: the main checkout must be excluded with zero reads.
    let api =
        CountingHost::new(FakeOpenCodeApi::empty().with_session("ses_probe", repo.dir.path()));
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "the main checkout is never removed");
    assert!(repo.dir.path().exists());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == repo.dir.path().to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "skipped_protected");
    assert_eq!(
        action.message.as_deref(),
        Some("main checkout is never removed"),
        "the main checkout reports its local reason"
    );
    assert_eq!(
        api.reads(),
        0,
        "the main checkout is excluded with no host or provider read"
    );
    let row = list_all_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-main")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RETAINED);
}

#[test]
fn prune_keeps_an_active_peer_path() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-peer") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-peer");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-peer01");
    let target_path = target.to_string_lossy().to_string();
    lease(
        &storage,
        "lease-peer",
        &identity,
        "session-peer",
        LEASE_STATUS_ACTIVE,
        &target_path,
    );
    lease(
        &storage,
        "lease-candidate",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target_path,
    );
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "an active peer path stays");
    assert!(target.exists());
    assert!(
        api.moves().is_empty(),
        "an active lease never reaches the removal stage"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target_path)
        .expect("action");
    assert_eq!(action.result, "skipped_active");
    assert_eq!(action.lease_ids.len(), 2, "both rows share one directory");
}

#[test]
fn prune_keeps_a_directory_with_a_same_owner_active_row() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-sameowner") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-sameowner");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-sameown1");
    let target_path = target.to_string_lossy().to_string();
    // Both rows belong to the same session: an active reference still blocks.
    lease(
        &storage,
        "lease-own-active",
        &identity,
        "session-A",
        LEASE_STATUS_ACTIVE,
        &target_path,
    );
    lease(
        &storage,
        "lease-own-retained",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target_path,
    );
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "same-owner active rows block removal");
    assert!(target.exists());
}

#[test]
fn prune_keeps_a_directory_when_an_active_row_is_beyond_the_row_ceiling() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-beyond") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-beyond");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-beyond01");
    let target_path = target.to_string_lossy().to_string();
    // 300 absent-history rows push the active reference beyond the capped
    // query's 256-row ceiling.
    for index in 0..300 {
        lease(
            &storage,
            &format!("lease-hist-{index:03}"),
            &identity,
            "session-A",
            LEASE_STATUS_RELEASED,
            &format!("/tmp/phasegent-916-history-{index:03}"),
        );
    }
    lease(
        &storage,
        "lease-beyond-active",
        &identity,
        "session-A",
        LEASE_STATUS_ACTIVE,
        &target_path,
    );
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an active row beyond row 256 must still block"
    );
    assert!(target.exists());
}

#[test]
fn prune_keeps_a_running_opencode_directory() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-hosted") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-hosted");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-hosted1");
    lease(
        &storage,
        "lease-hosted",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &target)
        .with_running("ses_a");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a running directory stays");
    assert!(target.exists());
    assert!(api.moves().is_empty(), "prune never moves a session");
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "skipped_protected");
}

#[test]
fn prune_keeps_a_directory_when_the_active_query_fails() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-incomplete") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-incomplete");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-incompl1");
    lease(
        &storage,
        "lease-incomplete",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", repo.dir.path())
        .failing_active();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an unreadable running-session answer keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

/// Issue 1061 P1: an associated idle session is returned to the verified main
/// checkout before the directory it sits in is removed, so the session never
/// keeps pointing at a directory that is gone.
#[test]
fn prune_returns_an_associated_idle_session_before_removing_its_directory() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-vacant") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-vacant");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-vacant01");
    lease(
        &storage,
        "lease-vacant",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // The session is hosted in the candidate but the service reports nothing
    // running, so it is idle and may be returned to the main checkout.
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &target);
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 1,
        "an idle associated session does not keep the directory"
    );
    assert!(!target.exists());
    assert_eq!(
        api.moves(),
        vec![(
            "ses_a".to_owned(),
            repo.dir.path().to_string_lossy().into_owned()
        )],
        "the associated idle session is returned to the verified main checkout"
    );
    let located = api.session("ses_a").expect("the moved session");
    assert!(
        located.directory == repo.dir.path().to_string_lossy(),
        "the session now runs in the main checkout: {}",
        located.directory
    );
    let row = list_all_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-vacant")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RELEASED);
}

/// Issue 1061 P1: an associated session that already runs elsewhere is left
/// exactly where it is, and its directory is still removable.
#[test]
fn prune_leaves_an_associated_session_that_is_elsewhere_untouched() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-elsewhere") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-elsewhere");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-elsew001");
    lease(
        &storage,
        "lease-elsewhere",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // The session already runs in the main checkout, so nothing is moved.
    let api = FakeOpenCodeApi::empty().with_session("ses_a", repo.dir.path());
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 1, "the directory is free and is removed");
    assert!(!target.exists());
    assert!(api.moves().is_empty(), "a session elsewhere is never moved");
}

/// Issue 1061 P1: several associated idle sessions are all returned to the
/// verified main checkout before the directory is removed.
#[test]
fn prune_returns_every_associated_idle_session() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-multi") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-multi");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-multi001");
    lease(
        &storage,
        "lease-multi-a",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    lease(
        &storage,
        "lease-multi-b",
        &identity,
        "ses_b",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &target)
        .with_session("ses_b", &target);
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 1, "both idle sessions leave the directory");
    assert!(!target.exists());
    let main = repo.dir.path().to_string_lossy().into_owned();
    assert_eq!(
        api.moves(),
        vec![
            ("ses_a".to_owned(), main.clone()),
            ("ses_b".to_owned(), main)
        ],
        "every associated idle session is returned to the main checkout"
    );
}

/// Issue 1061 P1: a session no lease of this repository associates is never
/// moved, and the directory it occupies is kept.
#[test]
fn prune_keeps_a_directory_hosted_by_a_foreign_idle_session() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-foreign") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-foreign");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-foreign01");
    lease(
        &storage,
        "lease-foreign",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // A foreign session sits in the candidate; it is idle, but it is not ours.
    let api = FakeOpenCodeApi::empty().with_session("ses_foreign", &target);
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a foreign occupant keeps the directory");
    assert!(target.exists());
    assert!(api.moves().is_empty(), "a foreign session is never moved");
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "skipped_protected");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("not associated with this repository"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1: a move the host accepts but does not perform leaves the
/// session where it was, so the directory must survive.
#[test]
fn prune_keeps_a_directory_when_the_move_is_only_queued() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-queued") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-queued");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-queued01");
    lease(
        &storage,
        "lease-queued",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &target)
        .queueing_moves();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a queued move keeps the directory");
    assert!(target.exists());
    assert_eq!(api.moves().len(), 1, "the move was requested");
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("has not left"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1: a rejected move keeps the directory.
#[test]
fn prune_keeps_a_directory_when_the_move_is_rejected() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-rejected") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-rejected");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-reject01");
    lease(
        &storage,
        "lease-rejected",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &target)
        .failing_move();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a rejected move keeps the directory");
    assert!(target.exists());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("moving the associated OpenCode session failed"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1 (attempt 2): a direct answer that names another session is
/// not evidence about the session that was asked for, and it must never
/// authorise a move of the id it happens to carry.
#[test]
fn prune_rejects_a_mismatched_associated_session_before_any_move() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-mismatch") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-mismatch");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-mismatch1");
    lease(
        &storage,
        "lease-mismatch",
        &identity,
        "ses_owner",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // The listing omits the associated owner — a host whose own directory
    // scoping under-reports — and the direct answer for it names another
    // session. Nothing is running, so only the identity check can catch it.
    let api = MismatchedIdHost::new(
        FakeOpenCodeApi::empty()
            .with_session("ses_owner", &target)
            .hidden_from_list("ses_owner"),
        "ses_owner",
    );
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a mismatched direct answer keeps the directory"
    );
    assert!(target.exists());
    assert!(
        api.inner.moves().is_empty(),
        "an answer for another session never authorises a move"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("with a different session"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1 (attempt 2): the move confirmation must answer for the
/// session that was moved; an answer naming another session proves nothing
/// about the move.
#[test]
fn prune_keeps_a_directory_when_the_move_confirmation_names_another_session() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-confirm") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-confirm");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-confirm1");
    lease(
        &storage,
        "lease-confirm",
        &identity,
        "ses_owner",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = MismatchedAfterMoveHost::new(
        FakeOpenCodeApi::empty().with_session("ses_owner", &target),
        "ses_owner",
    );
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an untrustworthy move confirmation keeps the directory"
    );
    assert!(target.exists());
    assert_eq!(
        api.inner.moves().len(),
        1,
        "the move was requested before the confirmation was read"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("with a different session"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1 (attempt 3): a direct answer whose location cannot be read is
/// not evidence that the session is elsewhere, so the directory is kept and
/// nothing is moved.
#[test]
fn prune_rejects_a_relative_associated_location_before_any_move() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-relative") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-relative");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-relative1");
    lease(
        &storage,
        "lease-relative",
        &identity,
        "ses_owner",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // The listing omits the associated owner and the direct answer carries
    // the right id but a location that cannot be read. Nothing is running, so
    // only the location check can catch it.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", std::path::Path::new("relative/unknown"))
        .hidden_from_list("ses_owner");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an unusable associated location keeps the directory"
    );
    assert!(target.exists());
    assert!(
        api.moves().is_empty(),
        "an unusable location never authorises a move"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("non-absolute location"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1 (attempt 3): the move confirmation must report a location
/// that can be read; an unusable one proves nothing about the move.
#[test]
fn prune_keeps_a_directory_when_the_move_confirmation_location_is_unusable() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-relative2") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-relative2");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-relative2");
    lease(
        &storage,
        "lease-relative-confirm",
        &identity,
        "ses_owner",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // The first read after the move — the confirmation — answers with a
    // location that cannot be read.
    let api = RelativeAfterMoveHost::new(
        FakeOpenCodeApi::empty().with_session("ses_owner", &target),
        "ses_owner",
        0,
    );
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an unusable move confirmation keeps the directory"
    );
    assert!(target.exists());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("non-absolute location"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1 (attempt 3): the post-move vacancy read must also report a
/// location that can be read, so a malformed answer keeps the directory.
#[test]
fn prune_keeps_a_directory_when_the_post_move_vacancy_location_is_unusable() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-relative3") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-relative3");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-relative3");
    lease(
        &storage,
        "lease-relative-vacancy",
        &identity,
        "ses_owner",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // The confirmation succeeds; the vacancy read that follows answers with a
    // location that cannot be read.
    let api = RelativeAfterMoveHost::new(
        FakeOpenCodeApi::empty().with_session("ses_owner", &target),
        "ses_owner",
        1,
    );
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an unusable vacancy location keeps the directory"
    );
    assert!(target.exists());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("non-absolute location"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1: a session that starts running after the scan is caught by
/// the relocation stage's own running check, and nothing is moved.
#[test]
fn prune_keeps_a_directory_when_a_session_starts_running_at_the_relocation_stage() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-running") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-running");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-running1");
    lease(
        &storage,
        "lease-running",
        &identity,
        "ses_owner",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = RunningAfterDiscoveryHost::new(
        FakeOpenCodeApi::empty()
            .with_session("ses_owner", &target)
            .with_running("ses_owner"),
    );
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a session that started running keeps the directory"
    );
    assert!(target.exists());
    assert!(
        api.inner.moves().is_empty(),
        "a running session is never moved"
    );
    assert!(
        api.listings.get() > 0,
        "the relocation stage reached its own occupancy discovery"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("currently occupies"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1: an occupant that appears while the associated sessions are
/// being returned keeps the directory.
#[test]
fn prune_keeps_a_directory_when_a_new_occupant_appears_while_the_sessions_return() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-incoming") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-incoming");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-incoming1");
    lease(
        &storage,
        "lease-incoming",
        &identity,
        "ses_owner",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // The discovery listing is clean; a new occupant shows up in the fresh
    // listing that follows the move.
    let api = NewOccupantHost::new(
        FakeOpenCodeApi::empty().with_session("ses_owner", &target),
        LocatedSession {
            id: "ses_new".to_owned(),
            directory: target.to_string_lossy().into_owned(),
        },
    );
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a new occupant keeps the directory");
    assert!(target.exists());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("1 session(s) are still hosted here"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1: an associated session the selected instance does not own is
/// uncertainty, so the directory is kept.
#[test]
fn prune_keeps_a_directory_when_an_associated_session_cannot_be_located() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-unknown") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-unknown");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-unknown1");
    lease(
        &storage,
        "lease-unknown",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &target)
        .with_unknown_session("ses_a");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an unlocatable associated session keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

/// Issue 1061 P1: an incomplete occupancy listing is not evidence of an empty
/// directory, so the directory is kept.
#[test]
fn prune_keeps_a_directory_when_the_occupancy_listing_is_incomplete() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-listing") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-1061-listing");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-listing1");
    lease(
        &storage,
        "lease-listing",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &target)
        .failing_list();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an incomplete listing keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("occupancy listing is incomplete"),
        "got: {:?}",
        action.message
    );
}

/// Issue 1061 P1: without a verified main checkout there is nowhere to return
/// a session to, so the directory is kept.
#[test]
fn prune_keeps_a_directory_when_no_main_checkout_can_be_verified() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-1061-target") else {
        return;
    };
    let elsewhere = TempDir::new("prune-1061-not-a-repo");
    let (_temp, storage, _env) = open_temp_db("prune-1061-target");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/1061-target01");
    lease(
        &storage,
        "lease-target",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &target);
    let source = ScriptedIssueStateSource::closed();
    let rows = list_all_for_repo(&storage, &identity).expect("list");
    let pinned = pinned_active_paths(&rows);
    // The scope's checkout is not a Git repository, so no main checkout can be
    // verified and there is no target to return the session to.
    let resolver = TargetResolver::new(elsewhere.path(), &identity);
    let scope = PruneScope {
        identity: &identity,
        repo_path: elsewhere.path(),
        rows: &rows,
        target: &resolver,
    };
    let summary = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &scope,
        &pinned,
        &source,
        999,
        PruneMode::Remove,
        &|_| {},
    );
    assert_eq!(
        summary.removed, 0,
        "an unverifiable main checkout keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("no verified main checkout"),
        "got: {:?}",
        action.message
    );
}

#[test]
fn prune_keeps_a_directory_whose_issue_is_not_closed() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-open") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-open");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-open0001");
    lease(
        &storage,
        "lease-open",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty();
    // The default answers "issue state 'Open' is not closed".
    let source = ScriptedIssueStateSource::keeping("issue 7 state 'Open' is not closed");
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "an open issue keeps the directory");
    assert!(target.exists());
    assert_eq!(
        api.list_calls(),
        0,
        "an unclosed issue never reaches the removal stage"
    );
    assert!(api.moves().is_empty());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "skipped_issue");
    assert_eq!(summary.kept_issue, 1);
    // The source is only asked once per directory.
    assert_eq!(source.asked().len(), 1);
}

/// Issue 929 P1: the removal recheck refreshes the source, so an issue that the
/// provider reported closed during the scan but has since reopened vetoes the
/// removal (and the scan answer alone is never enough).
#[test]
fn prune_reopen_between_scan_and_recheck_vetoes_removal() {
    use std::cell::Cell;

    /// Closed for the scan, then keeping once refreshed, so the only closure
    /// read that could authorize the deletion is the refreshed one.
    struct ReopeningSource {
        refreshed: Cell<bool>,
        refreshes: Cell<usize>,
    }
    impl IssueStateSource for ReopeningSource {
        fn closure(&self, _repo_path: &std::path::Path, _issue: u64) -> IssueClosure {
            if self.refreshed.get() {
                IssueClosure::Keep("issue 7 was reopened".to_owned())
            } else {
                IssueClosure::Closed
            }
        }
        fn refresh(&self) {
            self.refreshes.set(self.refreshes.get() + 1);
            self.refreshed.set(true);
        }
    }

    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-929-reopen") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-929-reopen");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/929-reopen1");
    lease(
        &storage,
        "lease-reopen",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty();
    let source = ReopeningSource {
        refreshed: Cell::new(false),
        refreshes: Cell::new(0),
    };
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a reopened issue must veto the removal");
    assert!(target.exists(), "the directory must survive the reopen");
    assert_eq!(source.refreshes.get(), 1, "the recheck refreshes once");
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "skipped_protected");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("reopened"),
        "got: {:?}",
        action.message
    );
}

#[test]
fn prune_requires_every_associated_issue_closed() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-multi") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-multi");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-multi001");
    let target_path = target.to_string_lossy().to_string();
    lease_for_issue(
        &storage,
        "lease-issue-1",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target_path,
        1,
    );
    lease_for_issue(
        &storage,
        "lease-issue-2",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target_path,
        2,
    );
    let api = FakeOpenCodeApi::empty();
    // Issue 1 is closed; issue 2 is still open.
    let source = ScriptedIssueStateSource::closed()
        .with_issue(1, IssueClosure::Closed)
        .with_issue(
            2,
            IssueClosure::Keep("issue 2 state 'Open' is not closed".to_owned()),
        );
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "every associated issue must be closed");
    assert!(target.exists());

    // Both closed: the directory becomes removable.
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 1,
        "both issues closed removes the directory"
    );
    assert!(!target.exists());
}

#[test]
fn prune_counts_and_removes_a_shared_path_once() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-dedup") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-dedup");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-dedup01");
    let target_path = target.to_string_lossy().to_string();
    // Two historical rows over the same canonical directory.
    lease(
        &storage,
        "lease-dedup-a",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target_path,
    );
    lease(
        &storage,
        "lease-dedup-b",
        &identity,
        "session-B",
        LEASE_STATUS_RELEASED,
        &target_path,
    );
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.scanned, 1, "one canonical directory, one candidate");
    assert_eq!(summary.removed, 1, "removed exactly once");
    assert!(!target.exists());
    let rows = list_all_for_repo(&storage, &identity).expect("list");
    for lease_id in ["lease-dedup-a", "lease-dedup-b"] {
        let row = rows
            .iter()
            .find(|row| row.lease_id == lease_id)
            .expect("row");
        assert_eq!(row.status, LEASE_STATUS_RELEASED);
    }
}

/// The dry-run preview queries the running-session set read-only, but it never
/// moves a session, flips a lease, or deletes a directory.
#[test]
fn prune_preview_discovers_the_host_read_only_and_never_mutates() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-guard-preview") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-guard-preview");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/916-preview1");
    lease(
        &storage,
        "lease-preview",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &target)
        .with_running("ses_a");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Report);
    assert!(summary.dry_run);
    assert_eq!(summary.removed, 0);
    assert!(target.exists(), "preview must not delete a worktree");
    assert!(api.moves().is_empty(), "preview must never move a session");
    let row = list_all_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-preview")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RETAINED);
}

// ---------------------------------------------------------------------------
// Global scope (issue 913/916): the default pass covers every stored identity.
// ---------------------------------------------------------------------------

/// The default (no `--repo`) pass enumerates every repository identity the
/// local lease store records and cleans an eligible directory from each, even
/// when the current directory is outside Git.
#[test]
fn prune_global_default_cleans_every_stored_identity_outside_git() {
    let _lock = lock_workflow_tests();
    let Some(repo_a) = TempRepo::init("prune-global-a") else {
        return;
    };
    let Some(repo_b) = TempRepo::init("prune-global-b") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-global");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let id_a = repo_identity(&runner, repo_a.dir.path()).expect("identity a");
    let id_b = repo_identity(&runner, repo_b.dir.path()).expect("identity b");
    let wt_a = add_real_worktree(&repo_a, "phasegent/916-globala");
    let wt_b = add_real_worktree(&repo_b, "phasegent/916-globalb");
    lease(
        &storage,
        "lease-a",
        &id_a,
        "session-A",
        LEASE_STATUS_RETAINED,
        &wt_a.to_string_lossy(),
    );
    lease(
        &storage,
        "lease-b",
        &id_b,
        "session-B",
        LEASE_STATUS_RETAINED,
        &wt_b.to_string_lossy(),
    );
    // A plain directory with no Git repo proves the default pass does not
    // depend on being run from a checkout.
    let non_git = TempDir::new("prune-global-nongit");
    let source = ScriptedIssueStateSource::closed();
    let exit = in_dir(non_git.path(), || {
        run_global_prune(999, false, "json", &source, &FakeOpenCodeApi::empty())
    });
    assert_eq!(exit, 0, "the global default pass must succeed");
    assert!(!wt_a.exists(), "repo A's eligible worktree is removed");
    assert!(!wt_b.exists(), "repo B's eligible worktree is removed");
    for (id, lease_id) in [(&id_a, "lease-a"), (&id_b, "lease-b")] {
        let row = list_all_for_repo(&storage, id)
            .expect("list")
            .into_iter()
            .find(|row| row.lease_id == lease_id)
            .expect("row");
        assert_eq!(row.status, LEASE_STATUS_RELEASED);
    }
}

/// `--repo PATH` restricts the pass to that one verified checkout.
#[test]
fn prune_explicit_repo_touches_only_that_repository() {
    let _lock = lock_workflow_tests();
    let Some(repo_a) = TempRepo::init("prune-scoped-a") else {
        return;
    };
    let Some(repo_b) = TempRepo::init("prune-scoped-b") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-scoped");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let id_a = repo_identity(&runner, repo_a.dir.path()).expect("identity a");
    let id_b = repo_identity(&runner, repo_b.dir.path()).expect("identity b");
    let wt_a = add_real_worktree(&repo_a, "phasegent/916-scopea");
    let wt_b = add_real_worktree(&repo_b, "phasegent/916-scopeb");
    lease(
        &storage,
        "lease-a",
        &id_a,
        "session-A",
        LEASE_STATUS_RETAINED,
        &wt_a.to_string_lossy(),
    );
    lease(
        &storage,
        "lease-b",
        &id_b,
        "session-B",
        LEASE_STATUS_RETAINED,
        &wt_b.to_string_lossy(),
    );
    let source = ScriptedIssueStateSource::closed();
    let exit = run_scoped_prune(
        &repo_a,
        999,
        false,
        None,
        false,
        "json",
        &source,
        &FakeOpenCodeApi::empty(),
    );
    assert_eq!(exit, 0, "the scoped pass must succeed");
    assert!(!wt_a.exists(), "the named repository's worktree is removed");
    assert!(
        wt_b.exists(),
        "a repository outside --repo must be untouched"
    );
    let row_b = list_all_for_repo(&storage, &id_b)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-b")
        .expect("row");
    assert_eq!(row_b.status, LEASE_STATUS_RETAINED);
}

/// An unknown `--repo` path is an argument error before anything is touched.
#[test]
fn prune_rejects_a_missing_explicit_repo() {
    let _lock = lock_workflow_tests();
    let (_temp, _storage, _env) = open_temp_db("prune-missing-repo");
    let source = ScriptedIssueStateSource::closed();
    let exit = execute_prune_with_source(
        Some("/nonexistent-phasegent-916-repo"),
        999,
        false,
        None,
        false,
        "json",
        &source,
        &crate::worktree::opencode::ProcessOpenCodeApi::new(),
        &|_| {},
    );
    assert_eq!(exit, 2, "a missing --repo path must fail as an argument");
}

// ---------------------------------------------------------------------------
// Issue 928 P1: prune blocks only on genuinely running sessions from the
// connected shared service, never on inactive historical root/child records,
// and it never moves a session.
// ---------------------------------------------------------------------------

/// A retained lease for `session` on a fresh real worktree, so the candidate is
/// a persisted `ses_` owner and prune's guards evaluate it.
fn owner_lease(storage: &Storage, identity: &str, target: &std::path::Path, session: &str) {
    lease(
        storage,
        "lease-owner",
        identity,
        session,
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
}

#[test]
fn prune_keeps_a_running_root_session_at_the_target() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-running-root") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-running-root");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-root0001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The owner is a running session currently in the candidate.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", &target)
        .with_running("ses_owner");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a running root keeps the directory");
    assert!(target.exists());
    assert!(api.moves().is_empty(), "prune never moves a session");
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "skipped_protected");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("currently occupies"),
        "got: {:?}",
        action.message
    );
}

#[test]
fn prune_keeps_a_running_child_session_at_the_target() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-running-child") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-running-child");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-child000");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // A child session is running in the candidate, even though the owner moved
    // on; a running child still blocks removal.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_session("ses_child", &target)
        .with_running("ses_child");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a running child keeps the directory");
    assert!(target.exists());
    assert!(api.moves().is_empty(), "prune never moves a session");
}

#[test]
fn prune_removes_when_the_running_session_is_elsewhere() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-running-elsewhere") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-running-elsewhere");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-elsewh01");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The owner is running, but at the main checkout, not the candidate.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_running("ses_owner");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 1,
        "a session running elsewhere permits removal"
    );
    assert!(!target.exists());
    assert!(api.moves().is_empty(), "prune never moves a session");
}

/// Issue 1061 P1: the read-only scan never enumerates history and never moves
/// a session; only the removal stage reads the occupancy of the directory it
/// is about to remove.
#[test]
fn prune_scan_never_enumerates_history_and_only_the_removal_stage_does() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-nohistory") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-nohistory");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-nohist1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The scan host is a separate view, so its reads can be told apart from
    // the removal stage's.
    let scan_host = CountingHost::new(FakeOpenCodeApi::empty().with_session("ses_owner", &target));
    let fresh_host = FakeOpenCodeApi::empty().with_session("ses_owner", &target);
    let rows = list_all_for_repo(&storage, &identity).expect("list");
    let pinned = pinned_active_paths(&rows);
    let target_resolver = TargetResolver::new(repo.dir.path(), &identity);
    let scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        rows: &rows,
        target: &target_resolver,
    };
    let source = ScriptedIssueStateSource::closed();
    let summary = prune_pass(
        &storage,
        &runner,
        &scan_host,
        &fresh_host,
        &scope,
        &pinned,
        &source,
        999,
        PruneMode::Remove,
        &|_| {},
    );
    assert_eq!(summary.removed, 1);
    assert_eq!(
        scan_host.lists(),
        0,
        "the scan must never enumerate historical sessions"
    );
    assert!(
        scan_host.moves().is_empty(),
        "the scan never moves a session"
    );
    assert!(
        fresh_host.list_calls() > 0,
        "the removal stage reads the occupancy of the directory it removes"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action.relocated_children.is_empty() && action.planned_children.is_empty(),
        "the JSON compatibility fields stay empty"
    );
}

#[test]
fn prune_keeps_when_the_running_session_query_is_unsupported() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-unsupported") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-unsupported");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-unsupp01");
    lease(
        &storage,
        "lease-plain",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // A host with no running-session capability: the query fails closed, so even
    // a pure-CLI candidate is kept.
    let api = NoActiveHost {
        inner: FakeOpenCodeApi::empty(),
    };
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an unsupported running-session query keeps the directory"
    );
    assert!(target.exists());
}

#[test]
fn prune_keeps_a_running_session_reported_under_a_canonical_alias() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-alias") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-alias");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-alias001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The running session's directory is spelled differently but resolves to the
    // same canonical directory, so the alias must still protect it.
    let alias = target.join(".");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", &alias)
        .with_running("ses_owner");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a canonical alias protects the directory"
    );
    assert!(target.exists());
}

#[test]
fn prune_keeps_when_a_running_session_id_is_unlocatable() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-unlocatable") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-unlocatable");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-unloc0001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The active query names an id the host will not describe: the answer is
    // invalid, so the directory is kept.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_running("ses_missing");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an unlocatable running id keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn prune_keeps_when_a_running_session_id_is_mismatched() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-mismatch") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-mismatch");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-mismat01");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The active query names `ses_m`, but `session.get` answers with a different
    // id: the answer is invalid, so the directory is kept.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_session("ses_m", repo.dir.path())
        .with_mismatched_running_id("ses_m");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a mismatched running answer keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn prune_keeps_when_a_running_session_reports_a_non_absolute_directory() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-notabsolute") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-notabsolute");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-notabs001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // A running session whose reported directory is relative is not a usable
    // location, so the whole answer is invalid and the directory is kept.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_session("ses_rel", std::path::Path::new("relative/dir"))
        .with_running("ses_rel");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a non-absolute running location keeps the directory"
    );
    assert!(target.exists());
}

#[test]
fn prune_keeps_a_running_foreign_session_at_the_target() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-foreign-running") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-foreign-running");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-foreign01");
    // A pure-CLI candidate (no `ses_` lease) is still protected by a manually
    // opened running host session that sits in the directory.
    lease(
        &storage,
        "lease-plain",
        &identity,
        "session-A",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_foreign", &target)
        .with_running("ses_foreign");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a running foreign session protects the path"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty(), "prune never moves a session");
}

#[test]
fn prune_new_running_session_on_recheck_vetoes_deletion() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-stale") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-stale");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-stale001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The scan sees nothing running; the fresh host finds the session running in
    // the candidate, so the fresh proof vetoes the deletion.
    let scan_host = FakeOpenCodeApi::empty().with_session("ses_owner", &target);
    let fresh_host = FakeOpenCodeApi::empty()
        .with_session("ses_owner", &target)
        .with_running("ses_owner");
    let rows = list_all_for_repo(&storage, &identity).expect("list");
    let pinned = pinned_active_paths(&rows);
    let target_resolver = TargetResolver::new(repo.dir.path(), &identity);
    let scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        rows: &rows,
        target: &target_resolver,
    };
    let source = ScriptedIssueStateSource::closed();
    let summary = prune_pass(
        &storage,
        &runner,
        &scan_host,
        &fresh_host,
        &scope,
        &pinned,
        &source,
        999,
        PruneMode::Remove,
        &|_| {},
    );
    assert_eq!(
        summary.removed, 0,
        "a fresh running session vetoes the stale scan plan"
    );
    assert!(target.exists());
    assert!(fresh_host.moves().is_empty());
    assert!(scan_host.moves().is_empty());
}

#[test]
fn prune_incoming_running_occupant_on_recheck_keeps_the_directory() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-incoming") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-incoming");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-incomi01");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let scan_host = FakeOpenCodeApi::empty().with_session("ses_owner", &target);
    // A new running session appears in the candidate only in the fresh read.
    let fresh_host = FakeOpenCodeApi::empty()
        .with_session("ses_owner", &target)
        .with_session("ses_new", &target)
        .with_running("ses_new");
    let rows = list_all_for_repo(&storage, &identity).expect("list");
    let pinned = pinned_active_paths(&rows);
    let target_resolver = TargetResolver::new(repo.dir.path(), &identity);
    let scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        rows: &rows,
        target: &target_resolver,
    };
    let source = ScriptedIssueStateSource::closed();
    let summary = prune_pass(
        &storage,
        &runner,
        &scan_host,
        &fresh_host,
        &scope,
        &pinned,
        &source,
        999,
        PruneMode::Remove,
        &|_| {},
    );
    assert_eq!(
        summary.removed, 0,
        "an incoming running occupant keeps the directory"
    );
    assert!(target.exists());
    assert!(fresh_host.moves().is_empty());
}

#[test]
fn prune_preview_reports_prunable_without_mutating() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-preview") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-preview");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-previe01");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // Only inactive history points at the candidate; nothing is running.
    let api = FakeOpenCodeApi::empty().with_session("ses_owner", &target);
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Report);
    assert_eq!(summary.removed, 0);
    assert!(target.exists(), "preview never deletes a directory");
    assert!(api.moves().is_empty(), "preview never moves a session");
    assert_eq!(
        api.list_calls(),
        0,
        "preview never enumerates historical sessions"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.result == "prunable")
        .expect("a proposed removal");
    assert!(
        action.relocated_children.is_empty() && action.planned_children.is_empty(),
        "preview never reports a relocation"
    );
    let row = list_all_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-owner")
        .expect("row");
    assert_eq!(
        row.status, LEASE_STATUS_RETAINED,
        "preview never flips a lease"
    );
}

#[test]
fn prune_keeps_a_directory_with_two_running_sessions() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-two-running") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-two-running");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-two00001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_a", &target)
        .with_running("ses_a")
        .with_session("ses_b", &target)
        .with_running("ses_b");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "two running sessions keep the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

/// A `ses_`-leased dirty directory is excluded by the local guard before any
/// host read, with the accurate local reason and zero host calls.
#[test]
fn prune_keeps_a_dirty_directory_with_zero_host_reads() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-928-dirty") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-928-dirty");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/928-dirty001");
    std::fs::write(target.join("scratch.txt"), "scratch\n").expect("write scratch");
    lease(
        &storage,
        "lease-dirty",
        &identity,
        "ses_dirty",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    let api =
        CountingHost::new(FakeOpenCodeApi::empty().with_session("ses_dirty", repo.dir.path()));
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a dirty directory is kept");
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(
        action.message.as_deref(),
        Some("uncommitted or untracked files")
    );
    assert_eq!(
        api.reads(),
        0,
        "a dirty directory is kept with zero host reads"
    );
}
