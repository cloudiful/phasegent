use super::cli_support::*;
use super::support::*;
use super::*;

use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

/// A host wrapper that counts every read (`active_sessions`/`session`) it
/// forwards, so a test can prove a locally-excluded directory made no host call.
struct CountingHost {
    inner: FakeOpenCodeApi,
    reads: std::cell::Cell<usize>,
}

impl CountingHost {
    fn new(inner: FakeOpenCodeApi) -> Self {
        Self {
            inner,
            reads: std::cell::Cell::new(0),
        }
    }

    fn reads(&self) -> usize {
        self.reads.get()
    }
}

impl OpenCodeApi for CountingHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.reads.set(self.reads.get() + 1);
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.reads.set(self.reads.get() + 1);
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
    let scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        rows: &rows,
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
    source: &ScriptedIssueStateSource,
    mode: PruneMode,
) -> PruneSummary {
    let runner = ProcessWorktreeRunner::new();
    let rows = list_all_for_repo(storage, identity).expect("list");
    let pinned = pinned_active_paths(&rows);
    let scope = PruneScope {
        identity,
        repo_path: repo.dir.path(),
        rows: &rows,
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

#[test]
fn prune_removes_a_directory_with_only_inactive_historical_records() {
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
    // The session is a historical record pointing at the candidate, but the
    // service reports nothing running, so the directory is occupancy-free.
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &target);
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 1,
        "a directory with only inactive history is removed"
    );
    assert!(!target.exists());
    assert!(api.moves().is_empty(), "prune never moves a session");
    assert_eq!(
        api.list_calls(),
        0,
        "prune never enumerates historical sessions"
    );
    let row = list_all_for_repo(&storage, &identity)
        .expect("list")
        .into_iter()
        .find(|row| row.lease_id == "lease-vacant")
        .expect("row");
    assert_eq!(row.status, LEASE_STATUS_RELEASED);
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

#[test]
fn prune_never_enumerates_history_or_moves_a_session() {
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
    // Historical records point at the candidate, but nothing is running.
    let api = FakeOpenCodeApi::empty().with_session("ses_owner", &target);
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 1);
    assert_eq!(
        api.list_calls(),
        0,
        "prune must never enumerate historical sessions"
    );
    assert!(api.moves().is_empty(), "prune must never move a session");
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert!(
        action.relocated_children.is_empty() && action.planned_children.is_empty(),
        "prune never reports a relocation"
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
    let scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        rows: &rows,
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
    let scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        rows: &rows,
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
