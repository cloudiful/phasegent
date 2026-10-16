use super::cli_support::*;
use super::support::*;
use super::*;

use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

/// A host wrapper that counts every read (`session`/`list`) it forwards, so a
/// test can prove a locally-excluded directory made no host call.
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
}

/// A directory that looked removable at scan time but whose session is freshly
/// reported as hosted during the final recheck must be kept: the removal
/// authorization never trusts the scan snapshot.
#[test]
fn prune_final_recheck_uses_the_uncached_host_and_keeps_a_newly_hosted_path() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-final-fresh") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-final-fresh");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/917-fresh01");
    lease(
        &storage,
        "lease-fresh",
        &identity,
        "ses_a",
        LEASE_STATUS_RETAINED,
        &target.to_string_lossy(),
    );
    // The scan host says the session is elsewhere (vacant); the fresh host says
    // it is now hosted in the candidate.
    let scan_host = FakeOpenCodeApi::empty().with_session("ses_a", repo.dir.path());
    let fresh_host = FakeOpenCodeApi::empty().with_session("ses_a", &target);
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
        "a freshly hosted session keeps the directory"
    );
    assert!(target.exists());
    assert!(fresh_host.moves().is_empty());
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
fn prune_keeps_a_hosted_opencode_directory() {
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
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &target);
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a hosted directory stays");
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
fn prune_keeps_a_directory_when_the_host_listing_is_incomplete() {
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
}

#[test]
fn prune_removes_a_vacant_opencode_directory() {
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
    // The host reports the session authoritatively at the main checkout.
    let api = FakeOpenCodeApi::empty().with_session("ses_a", repo.dir.path());
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 1, "a proven-vacant directory is removed");
    assert!(!target.exists());
    assert!(api.moves().is_empty(), "prune never moves a session");
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

/// The dry-run preview discovers the host read-only (issue 918 P2) so it can
/// report proposed child relocations, but it never moves a session, flips a
/// lease, or deletes a directory.
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
    let api = FakeOpenCodeApi::empty().with_session("ses_a", &target);
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
        run_global_prune(999, false, "json", &source)
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
    let exit = run_scoped_prune(&repo_a, 999, false, None, false, "json", &source);
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
// Issues 918 P2 / 923 P1: retire proven-associated child sessions so a
// child-occupied worktree can be cleaned, without deleting a directory an
// unrelated or foreign session still uses and without treating execution
// outcome as a cleanup criterion.
// ---------------------------------------------------------------------------

/// A retained lease for `session` on a fresh real worktree, plus helpers to
/// script the host's owner/child answers.
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
fn prune_retires_an_associated_child_and_removes_the_directory() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-direct") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-direct");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-direct1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The owner is elsewhere; an associated child still occupies the directory.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 1,
        "the associated-child directory is removed"
    );
    assert!(!target.exists());
    let moves = api.moves();
    assert_eq!(moves.len(), 1, "exactly one child is relocated");
    assert_eq!(moves[0].0, "ses_child");
    assert_eq!(moves[0].1, repo.dir.path().to_string_lossy());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "pruned");
    assert_eq!(action.relocated_children, vec!["ses_child".to_owned()]);
}

#[test]
fn prune_retires_a_nested_associated_descendant_through_an_intermediate_parent() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-nested") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-nested");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-nested1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // gc -> mid -> owner: only the grandchild occupies the directory.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child(
            "ses_mid",
            std::path::Path::new("/tmp/phasegent-918-elsewhere"),
            "ses_owner",
        )
        .with_outcome("ses_mid", "succeeded")
        .with_child("ses_gc", &target, "ses_mid")
        .with_outcome("ses_gc", "succeeded");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 1,
        "a nested associated descendant is retired"
    );
    assert!(!target.exists());
    assert_eq!(
        api.moves(),
        vec![(
            "ses_gc".to_owned(),
            repo.dir.path().to_string_lossy().into_owned()
        )]
    );
}

#[test]
fn prune_retires_multiple_associated_children_together() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-multi-child") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-multi-child");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-multi1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_c1", &target, "ses_owner")
        .with_outcome("ses_c1", "succeeded")
        .with_child("ses_c2", &target, "ses_owner")
        .with_outcome("ses_c2", "succeeded");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 1);
    assert!(!target.exists());
    assert_eq!(api.moves().len(), 2, "both children are relocated");
    let action = summary
        .actions
        .iter()
        .find(|action| action.result == "pruned")
        .expect("action");
    assert_eq!(
        action.relocated_children,
        vec!["ses_c1".to_owned(), "ses_c2".to_owned()]
    );
}

#[test]
fn prune_never_moves_a_lease_owner_that_still_occupies_the_directory() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-owner") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-owner");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-owner001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let api = FakeOpenCodeApi::empty().with_session("ses_owner", &target);
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a hosted lease owner keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty(), "the owner is never moved");
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "skipped_protected");
}

#[test]
fn prune_retires_an_associated_child_regardless_of_outcome() {
    // Execution outcome is not a cleanup criterion: every outcome for an
    // otherwise-associable child is eligible, including a missing one.
    for outcome in [
        Some("succeeded"),
        Some("failed"),
        Some("interrupted"),
        Some("cancelled"),
        None,
    ] {
        let label = outcome.unwrap_or("missing");
        let _lock = lock_workflow_tests();
        let Some(repo) = TempRepo::init(&format!("prune-923-outcome-{label}")) else {
            return;
        };
        let (_temp, storage, _env) = open_temp_db(&format!("prune-923-outcome-{label}"));
        ensure_schema(&storage).expect("schema");
        let runner = ProcessWorktreeRunner::new();
        let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
        let target = add_real_worktree(&repo, &format!("phasegent/923-{label}1"));
        owner_lease(&storage, &identity, &target, "ses_owner");
        let mut api = FakeOpenCodeApi::empty()
            .with_session("ses_owner", repo.dir.path())
            .with_child("ses_child", &target, "ses_owner");
        if let Some(outcome) = outcome {
            api = api.with_outcome("ses_child", outcome);
        }
        let source = ScriptedIssueStateSource::closed();
        let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
        assert_eq!(
            summary.removed, 1,
            "a child with outcome {label} is eligible"
        );
        assert!(!target.exists());
        assert_eq!(
            api.moves(),
            vec![(
                "ses_child".to_owned(),
                repo.dir.path().to_string_lossy().into_owned()
            )],
            "the associated child is relocated for outcome {label}"
        );
    }
}

#[test]
fn prune_keeps_an_unassociated_child_with_zero_moves() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-923-unassociated") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-923-unassociated");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/923-unassoc1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // No `parentID`: the child cannot be proven to descend from the owner, so
    // no outcome can authorize a move.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_session("ses_child", &target)
        .with_outcome("ses_child", "succeeded");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "an unproven child keeps the directory");
    assert!(target.exists());
    assert!(api.moves().is_empty(), "no move without association");
}

#[test]
fn prune_keeps_a_child_with_no_parent_metadata() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-noparent") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-noparent");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-noparent1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // A session with no parentID is not a proven descendant.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_session("ses_orphan", &target)
        .with_outcome("ses_orphan", "succeeded");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a missing parent chain keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn prune_keeps_a_cyclic_parent_chain() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-cycle") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-cycle");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-cycle001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_a", &target, "ses_b")
        .with_outcome("ses_a", "succeeded")
        .with_child(
            "ses_b",
            std::path::Path::new("/tmp/phasegent-918-elsewhere"),
            "ses_a",
        )
        .with_outcome("ses_b", "succeeded");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a cyclic parent chain keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn prune_keeps_a_too_deep_parent_chain() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-deep") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-deep");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-deep0001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // child -> p1 -> ... -> p9 -> owner: 9 hops, beyond the 8-hop bound.
    let mut api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_p1")
        .with_outcome("ses_child", "succeeded");
    for level in 1..=9 {
        let parent = if level == 9 {
            "ses_owner".to_owned()
        } else {
            format!("ses_p{}", level + 1)
        };
        api = api
            .with_child(
                &format!("ses_p{level}"),
                std::path::Path::new("/tmp/phasegent-918-elsewhere"),
                &parent,
            )
            .with_outcome(&format!("ses_p{level}"), "succeeded");
    }
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "an over-deep chain keeps the directory");
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn prune_keeps_a_foreign_occupant_with_zero_moves() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-foreign") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-foreign");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-foreign1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // A session whose ancestry never reaches this directory's owner.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_other", &target, "ses_elsewhere")
        .with_outcome("ses_other", "succeeded");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a foreign occupant keeps the directory");
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn prune_keeps_a_directory_when_an_unreadable_child_blocks_it() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-unreadable") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-unreadable");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-unread1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded")
        .failing_session();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "an unreadable host keeps the directory");
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn prune_keeps_the_directory_when_an_ack_only_move_does_not_relocate() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-ack") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-ack");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-ack00001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The host accepts the move but does not relocate the child.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded")
        .queueing_moves();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "an unconfirmed move keeps the directory"
    );
    assert!(target.exists());
    assert_eq!(
        api.moves().len(),
        1,
        "the move was requested but not confirmed"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.result == "skipped_protected")
        .expect("action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("retirement"),
        "got: {:?}",
        action.message
    );
}

#[test]
fn prune_keeps_the_directory_when_the_move_is_rejected() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-reject") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-reject");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-reject01");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded")
        .failing_move();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a rejected move keeps the directory");
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn prune_outcome_change_between_scan_and_recheck_does_not_block_removal() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-923-stale") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-923-stale");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/923-stale001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The scan sees a `succeeded` child; the fresh host reports `failed`. The
    // association is unchanged, so the outcome change alone must not block.
    let scan_host = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded");
    let fresh_host = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "failed");
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
        summary.removed, 1,
        "an outcome change between scan and recheck does not alone block"
    );
    assert!(!target.exists());
    assert_eq!(
        fresh_host.moves(),
        vec![(
            "ses_child".to_owned(),
            repo.dir.path().to_string_lossy().into_owned()
        )]
    );
    assert!(scan_host.moves().is_empty());
}

#[test]
fn prune_stale_scan_defeated_by_an_incoming_occupant() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-incoming") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-incoming");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-incoming1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let scan_host = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded");
    let fresh_host = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded")
        .with_child("ses_new", &target, "ses_newparent")
        .with_outcome("ses_new", "succeeded");
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
        "an incoming occupant keeps the directory"
    );
    assert!(target.exists());
    assert!(fresh_host.moves().is_empty());
}

#[test]
fn prune_preview_proposes_child_retirement_without_moving_anything() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-preview") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-preview");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-preview1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Report);
    assert_eq!(summary.removed, 0);
    assert!(target.exists(), "preview never deletes a directory");
    assert!(api.moves().is_empty(), "preview never moves a session");
    let action = summary
        .actions
        .iter()
        .find(|action| action.result == "prunable")
        .expect("a proposed removal");
    assert_eq!(action.planned_children, vec!["ses_child".to_owned()]);
    assert!(
        action.relocated_children.is_empty(),
        "a preview proposal is never labelled an actual relocation"
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
fn prune_keeps_when_no_main_checkout_can_be_verified() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-notarget") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-notarget");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-notarget1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded");
    let rows = list_all_for_repo(&storage, &identity).expect("list");
    let pinned = pinned_active_paths(&rows);
    // A `--repo`-style path outside any Git repository cannot resolve a main
    // checkout, so the retirement target is unavailable and nothing moves.
    let no_checkout = TempDir::new("prune-918-notarget-scope");
    let scope = PruneScope {
        identity: &identity,
        repo_path: no_checkout.path(),
        rows: &rows,
    };
    let source = ScriptedIssueStateSource::closed();
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
        "an unavailable target keeps the directory"
    );
    assert!(target.exists());
    assert!(api.moves().is_empty());
}

#[test]
fn prune_keeps_a_mixed_eligible_and_foreign_directory_with_zero_moves() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-918-mixed") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-918-mixed");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/918-mixed001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // One eligible associated child plus one foreign occupant: the foreign one
    // blocks the whole directory, so the eligible child is NOT moved either.
    let api = FakeOpenCodeApi::empty()
        .with_session("ses_owner", repo.dir.path())
        .with_child("ses_child", &target, "ses_owner")
        .with_outcome("ses_child", "succeeded")
        .with_child("ses_other", &target, "ses_elsewhere")
        .with_outcome("ses_other", "succeeded");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(summary.removed, 0, "a foreign occupant keeps the directory");
    assert!(target.exists());
    assert!(
        api.moves().is_empty(),
        "no partial move happens when any occupant is ineligible"
    );
}

// ---------------------------------------------------------------------------
// Issue 919 P2: local safety prechecks precede host retirement, and relocation
// outcomes are reported faithfully.
// ---------------------------------------------------------------------------

/// A host wrapper that can fail the Nth listing call or a specific session's
/// move, so a test can drive a late-occupant/partial-relocation outcome without
/// a real host. Session reads and moves otherwise delegate unchanged.
struct ScriptedHost {
    inner: FakeOpenCodeApi,
    fail_list_after_move: bool,
    moved: std::cell::Cell<bool>,
    fail_move_for: Option<String>,
}

impl ScriptedHost {
    fn new(inner: FakeOpenCodeApi) -> Self {
        Self {
            inner,
            fail_list_after_move: false,
            moved: std::cell::Cell::new(false),
            fail_move_for: None,
        }
    }

    fn failing_list_after_move(mut self) -> Self {
        self.fail_list_after_move = true;
        self
    }

    fn failing_move_for(mut self, session: &str) -> Self {
        self.fail_move_for = Some(session.to_owned());
        self
    }

    fn moves(&self) -> Vec<(String, String)> {
        self.inner.moves()
    }
}

impl OpenCodeApi for ScriptedHost {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        if self.fail_list_after_move && self.moved.get() {
            return Err(ApiError::plain("scripted late listing failure"));
        }
        self.inner.list(request)
    }

    fn move_session(&self, id: &str, directory: &std::path::Path) -> Result<(), ApiError> {
        if self.fail_move_for.as_deref() == Some(id) {
            return Err(ApiError::plain("scripted move rejection"));
        }
        self.inner.move_session(id, directory)?;
        self.moved.set(true);
        Ok(())
    }
}

/// A `ses_`-leased dirty directory is excluded by the local guard before any
/// host read, with the accurate local reason.
#[test]
fn prune_keeps_a_dirty_directory_with_zero_host_reads() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-919-dirty") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-919-dirty");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/919-dirty01");
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

/// A child moved and confirmed, but kept by a later occupancy check, is still
/// reported as an actual relocation (not a proposal).
#[test]
fn prune_reports_confirmed_moves_even_when_the_directory_is_later_kept() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-919-moved-kept") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-919-moved-kept");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/919-kept0001");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // The first listing (plan) and the child move succeed; the final occupancy
    // listing fails, so the directory is kept after the child was relocated.
    let api = ScriptedHost::new(
        FakeOpenCodeApi::empty()
            .with_session("ses_owner", repo.dir.path())
            .with_child("ses_child", &target, "ses_owner")
            .with_outcome("ses_child", "succeeded"),
    )
    .failing_list_after_move();
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "the directory is kept on the late check"
    );
    assert!(target.exists());
    assert_eq!(
        api.moves(),
        vec![(
            "ses_child".to_owned(),
            repo.dir.path().to_string_lossy().into_owned()
        )],
        "the child relocation happened"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "skipped_protected");
    assert_eq!(
        action.relocated_children,
        vec!["ses_child".to_owned()],
        "a confirmed move is reported as actual even though the directory was kept"
    );
    assert!(
        action.planned_children.is_empty(),
        "a removal pass never sets the proposal field"
    );
}

/// A partial relocation lists only the children whose move was confirmed.
#[test]
fn prune_reports_only_confirmed_children_when_a_later_move_fails() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-919-partial") else {
        return;
    };
    let (_temp, storage, _env) = open_temp_db("prune-919-partial");
    ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = add_real_worktree(&repo, "phasegent/919-partial1");
    owner_lease(&storage, &identity, &target, "ses_owner");
    // Two eligible children; the second move is rejected.
    let api = ScriptedHost::new(
        FakeOpenCodeApi::empty()
            .with_session("ses_owner", repo.dir.path())
            .with_child("ses_c1", &target, "ses_owner")
            .with_outcome("ses_c1", "succeeded")
            .with_child("ses_c2", &target, "ses_owner")
            .with_outcome("ses_c2", "succeeded"),
    )
    .failing_move_for("ses_c2");
    let source = ScriptedIssueStateSource::closed();
    let summary = run_pass(&storage, &repo, &identity, &api, &source, PruneMode::Remove);
    assert_eq!(
        summary.removed, 0,
        "a partial relocation keeps the directory"
    );
    assert!(target.exists());
    let action = summary
        .actions
        .iter()
        .find(|action| action.worktree_path == target.to_string_lossy())
        .expect("action");
    assert_eq!(action.result, "skipped_protected");
    assert_eq!(
        action.relocated_children,
        vec!["ses_c1".to_owned()],
        "only the confirmed move is listed"
    );
    assert!(
        action.planned_children.is_empty(),
        "a removal pass never sets the proposal field"
    );
}
