//! Integration protection for the prune removal stage's bounded mutation
//! windows: cache clearance cannot cache an absence, a reopened issue, or a
//! newly arrived foreign occupant.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use super::*;
use crate::cli::issue_closure::test_support::ScriptedIssueStateSource;
use crate::cli::issue_closure::{IssueClosure, IssueStateSource};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};
use crate::worktree::registry::{self, NewAssociation, NewDirectory};
use crate::worktree::{
    GitOutput, ProcessWorktreeRunner, WorktreeError, repo_identity, worktree_add,
};
use crate::worktree_tests::support::{TempDir, TempRepo, open_temp_db};

struct Fixture {
    repo: TempRepo,
    _db: TempDir,
    _env: crate::infra::storage::test_support::EnvGuard,
    _cache: TempDir,
    _cache_env: crate::infra::storage::test_support::EnvGuard,
    storage: Storage,
    identity: String,
    target: PathBuf,
}

fn fixture(label: &str) -> Option<Fixture> {
    let repo = TempRepo::init(label)?;
    let (db, storage, env) = open_temp_db(label);
    let cache = crate::worktree_tests::support::unique_cache(label);
    let cache_env = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    registry::ensure_schema(&storage).expect("registry schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = repo.dir.path().join("prune-target");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/prune-target")
        .expect("real worktree");
    Some(Fixture {
        repo,
        _db: db,
        _env: env,
        _cache: cache,
        _cache_env: cache_env,
        storage,
        identity,
        target,
    })
}

fn register(fixture: &Fixture, session: &str) {
    let canonical = crate::worktree::canonical_or_self(&fixture.target);
    let id = registry::stable_directory_id(&fixture.identity, &canonical.to_string_lossy());
    registry::insert_directory(
        &fixture.storage,
        &NewDirectory {
            directory_id: &id,
            repo_identity: &fixture.identity,
            checkout_path: &fixture.repo.dir.path().to_string_lossy(),
            worktree_path: &canonical.to_string_lossy(),
            branch: "phasegent/prune-target",
            created_at: 1,
            session,
            associations: &[NewAssociation::Legacy {
                issue: 1196,
                provider: "local".to_owned(),
                project: "default".to_owned(),
                external_id: 1196,
            }],
        },
    )
    .expect("registry row");
}

fn run(fixture: &Fixture, api: &dyn OpenCodeApi, source: &dyn IssueStateSource) -> PruneSummary {
    let runner = ProcessWorktreeRunner::new();
    let records = registry::load_records(&fixture.storage, Some(&fixture.identity)).expect("rows");
    let target = TargetResolver::new(fixture.repo.dir.path(), &fixture.identity);
    let scope = PruneScope {
        identity: &fixture.identity,
        repo_path: fixture.repo.dir.path(),
        records: &records,
        target: &target,
    };
    prune_pass(
        &fixture.storage,
        &runner,
        api,
        api,
        &scope,
        source,
        PruneMode::Remove,
        &|_| {},
    )
}

struct CountingApi {
    inner: FakeOpenCodeApi,
    active_calls: Cell<usize>,
    list_calls: Cell<usize>,
}

impl OpenCodeApi for CountingApi {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.inner.session(id)
    }
    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.list_calls.set(self.list_calls.get() + 1);
        self.inner.list(request)
    }
    fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }
    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.active_calls.set(self.active_calls.get() + 1);
        self.inner.active_sessions()
    }
}

#[test]
fn scan_reuses_snapshot_across_scopes_and_excludes_main() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("prune-shared-snapshot") else {
        return;
    };
    register(&fixture, "cli-session");
    let mut records = registry::load_records(&fixture.storage, Some(&fixture.identity)).unwrap();
    let mut main = records[0].clone();
    main.worktree_path = fixture.repo.dir.path().to_string_lossy().into_owned();
    records.push(main);
    let target = TargetResolver::new(fixture.repo.dir.path(), &fixture.identity);
    let scope = PruneScope {
        identity: &fixture.identity,
        repo_path: fixture.repo.dir.path(),
        records: &records,
        target: &target,
    };
    let api = CountingApi {
        inner: FakeOpenCodeApi::empty(),
        active_calls: Cell::new(0),
        list_calls: Cell::new(0),
    };
    let snapshot = std::cell::OnceCell::new();
    let runner = ProcessWorktreeRunner::new();
    let source = ScriptedIssueStateSource::closed();
    for _ in 0..2 {
        let candidates = scan_with_paths(&runner, &api, &scope, &source, &snapshot, false);
        assert_eq!(candidates.len(), 1);
        assert!(
            candidates.iter().all(|candidate| candidate.worktree_path
                != fixture.repo.dir.path().to_string_lossy().into_owned()),
            "the main checkout is never a candidate: {candidates:?}"
        );
    }
    assert_eq!(api.active_calls.get(), 1);
}

#[cfg(unix)]
#[test]
fn absent_probe_rejects_dangling_symlinks() {
    let dir = TempDir::new("prune-dangling-link");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(dir.path().join("missing"), &link).unwrap();
    assert!(path_absent(&link).is_err());
    assert!(path_absent(&link.join("child")).is_err());
    assert_eq!(path_absent(&dir.path().join("absent")), Ok(true));
}

#[test]
fn unavailable_repository_reclaims_only_absent_registrations() {
    let _lock = lock_workflow_tests();
    let (_db, storage, _env) = open_temp_db("prune-missing-root");
    let cache = crate::worktree_tests::support::unique_cache("prune-missing-root");
    let _cache_env = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    registry::ensure_schema(&storage).unwrap();
    let paths = TempDir::new("prune-missing-root-paths");
    let root = paths.path().join("missing-main");
    let identity = root.join(".git").to_string_lossy().into_owned();
    let absent = paths.path().join("missing-worktree");
    let existing = paths.path().join("existing-worktree");
    std::fs::create_dir(&existing).unwrap();
    for (id, path) in [
        ("absent", &absent),
        ("existing", &existing),
        // The main checkout of the unavailable repository is absent too, so
        // its registration is a stale row like any other.
        ("main", &root),
    ] {
        registry::insert_directory(
            &storage,
            &NewDirectory {
                directory_id: id,
                repo_identity: &identity,
                checkout_path: &root.to_string_lossy(),
                worktree_path: &path.to_string_lossy(),
                branch: "fixture",
                created_at: 1,
                session: "cli-session",
                associations: &[NewAssociation::Unscoped { issue: 1 }],
            },
        )
        .unwrap();
    }
    let records = registry::load_records(&storage, Some(&identity)).unwrap();
    let target = TargetResolver::new(&root, &identity);
    let scope = PruneScope {
        identity: &identity,
        repo_path: &root,
        records: &records,
        target: &target,
    };
    let api = CountingApi {
        inner: FakeOpenCodeApi::empty(),
        active_calls: Cell::new(0),
        list_calls: Cell::new(0),
    };
    let result = prune_pass_with_snapshot(
        &storage,
        &ProcessWorktreeRunner::new(),
        &api,
        &api,
        &scope,
        &ScriptedIssueStateSource::closed(),
        PruneMode::Remove,
        MovePolicy::NoWait,
        &|_| {},
        &std::cell::OnceCell::new(),
        true,
    );
    assert_eq!(
        result.absent, 2,
        "the absent main registration is reclaimed"
    );
    assert_eq!(result.kept_protected, 1);
    assert_eq!(api.active_calls.get(), 0);
    let remaining = registry::load_records(&storage, Some(&identity)).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].directory_id, "existing");
    assert!(existing.exists());
}

struct ReopensWhenCacheGone {
    worktree: PathBuf,
}

impl IssueStateSource for ReopensWhenCacheGone {
    fn closure(&self, _repo_path: &Path, _issue: u64) -> IssueClosure {
        if self.worktree.join(".cache").exists() {
            IssueClosure::Closed
        } else {
            IssueClosure::Keep("issue reopened after cache cleanup".to_owned())
        }
    }
}

struct ForeignAfterCacheGone {
    inner: FakeOpenCodeApi,
    worktree: PathBuf,
    listed: Cell<bool>,
}

impl OpenCodeApi for ForeignAfterCacheGone {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.inner.session(id)
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        let mut page = self.inner.list(request)?;
        if !self.worktree.join(".cache").exists() {
            self.listed.set(true);
            page.sessions.push(LocatedSession {
                id: "ses_foreign".to_owned(),
                directory: self.worktree.to_string_lossy().into_owned(),
                parent_id: None,
            });
        }
        Ok(page)
    }

    fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
        self.inner.move_session(id, directory)
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.inner.active_sessions()
    }
}

#[test]
fn prune_rechecks_closure_after_cache_cleanup() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("prune-post-cache-closure") else {
        return;
    };
    std::fs::create_dir_all(fixture.target.join(".cache/ai")).expect("cache dir");
    std::fs::write(fixture.target.join(".cache/ai/one.txt"), "scratch").expect("cache file");
    register(&fixture, "cli-session");
    let api = FakeOpenCodeApi::empty();
    let summary = run(
        &fixture,
        &api,
        &ReopensWhenCacheGone {
            worktree: fixture.target.clone(),
        },
    );
    assert_eq!(summary.removed, 0, "a reopened issue keeps the directory");
    assert!(fixture.target.exists());
    assert!(
        !fixture.target.join(".cache").exists(),
        "the cache was cleared first"
    );
    let action = summary
        .actions
        .iter()
        .find(|action| action.result == "skipped_protected")
        .expect("protected action");
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("kept after cache cleanup"),
        "got: {:?}",
        action.message
    );
}

#[test]
fn prune_keeps_foreign_idle_occupant_arriving_during_cache_cleanup() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("prune-post-cache-foreign") else {
        return;
    };
    std::fs::create_dir_all(fixture.target.join(".cache/ai")).expect("cache dir");
    std::fs::write(fixture.target.join(".cache/ai/one.txt"), "scratch").expect("cache file");
    register(&fixture, "cli-session");
    let api = ForeignAfterCacheGone {
        inner: FakeOpenCodeApi::empty(),
        worktree: fixture.target.clone(),
        listed: Cell::new(false),
    };
    let summary = run(&fixture, &api, &ScriptedIssueStateSource::closed());
    assert_eq!(
        summary.removed, 0,
        "foreign idle occupant must keep directory"
    );
    assert!(fixture.target.exists());
    assert!(api.listed.get(), "the post-cache host read did run");
}

#[test]
fn prune_checks_a_reappearing_absent_path_instead_of_caching_absence() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-reappearing-path") else {
        return;
    };
    let (_db, storage, _env) = open_temp_db("prune-reappearing-path");
    registry::ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let path = repo.dir.path().join("phasegent-reappearing");
    let id = registry::stable_directory_id(
        &identity,
        &crate::worktree::canonical_or_self(&path).to_string_lossy(),
    );
    registry::insert_directory(
        &storage,
        &NewDirectory {
            directory_id: &id,
            repo_identity: &identity,
            checkout_path: &repo.dir.path().to_string_lossy(),
            worktree_path: &path.to_string_lossy(),
            branch: "phasegent/reappearing",
            created_at: 1,
            session: "cli-session",
            associations: &[NewAssociation::Unscoped { issue: 1196 }],
        },
    )
    .expect("registry row");
    let api = FakeOpenCodeApi::empty();
    let source = ScriptedIssueStateSource::closed();
    let records = registry::load_records(&storage, Some(&identity)).expect("rows");
    let target = TargetResolver::new(repo.dir.path(), &identity);
    let first_scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        records: &records,
        target: &target,
    };
    let preview = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &first_scope,
        &source,
        PruneMode::Report,
        &|_| {},
    );
    assert_eq!(preview.absent, 1);
    assert_eq!(
        registry::load_records(&storage, Some(&identity)).unwrap(),
        records
    );
    let raced = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &first_scope,
        &source,
        PruneMode::Remove,
        &|stage| {
            if matches!(stage, PruneStage::Verifying { .. }) {
                std::fs::create_dir_all(&path).unwrap();
            }
        },
    );
    assert_eq!(raced.kept_protected, 1);
    assert_eq!(
        registry::load_records(&storage, Some(&identity)).unwrap(),
        records
    );
    std::fs::remove_dir(&path).unwrap();
    let first = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &first_scope,
        &source,
        PruneMode::Remove,
        &|_| {},
    );
    assert_eq!(first.absent, 1);
    assert!(
        registry::load_records(&storage, Some(&identity))
            .unwrap()
            .is_empty()
    );
    std::fs::create_dir_all(&path).expect("reappearing directory");
    let second_scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        records: &records,
        target: &target,
    };
    let second = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &second_scope,
        &source,
        PruneMode::Remove,
        &|_| {},
    );
    assert_eq!(
        second.absent, 0,
        "the reappeared path is no longer cached absent"
    );
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn prune_keeps_the_open_scope_of_a_same_number_multi_provider_record() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("prune-multi-provider") else {
        return;
    };
    let (_db, storage, _env) = open_temp_db("prune-multi-provider");
    registry::ensure_schema(&storage).expect("schema");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let path = repo.dir.path().join("multi-provider");
    std::fs::create_dir_all(&path).expect("path");
    let id = registry::stable_directory_id(
        &identity,
        &crate::worktree::canonical_or_self(&path).to_string_lossy(),
    );
    let closed = NewAssociation::Legacy {
        issue: 42,
        provider: "forgejo".to_owned(),
        project: "owner/repo".to_owned(),
        external_id: 42,
    };
    let open = NewAssociation::Legacy {
        issue: 42,
        provider: "redmine".to_owned(),
        project: "23".to_owned(),
        external_id: 42,
    };
    registry::insert_directory(
        &storage,
        &NewDirectory {
            directory_id: &id,
            repo_identity: &identity,
            checkout_path: &repo.dir.path().to_string_lossy(),
            worktree_path: &path.to_string_lossy(),
            branch: "phasegent/multi-provider",
            created_at: 1,
            session: "cli-session",
            associations: &[closed, open],
        },
    )
    .expect("registry row");
    struct ScopedSource;
    impl IssueStateSource for ScopedSource {
        fn closure(&self, _repo_path: &Path, _issue: u64) -> IssueClosure {
            IssueClosure::Closed
        }
        fn scoped_closure(
            &self,
            _repo_path: &Path,
            _issue: u64,
            scope: Option<&crate::worktree::WorktreeScope>,
        ) -> IssueClosure {
            match scope.map(|scope| scope.provider()) {
                Some("redmine") => IssueClosure::Open("the redmine scope is still open".to_owned()),
                _ => IssueClosure::Closed,
            }
        }
    }
    let api = FakeOpenCodeApi::empty();
    let records = registry::load_records(&storage, Some(&identity)).expect("rows");
    let target = TargetResolver::new(repo.dir.path(), &identity);
    let scope = PruneScope {
        identity: &identity,
        repo_path: repo.dir.path(),
        records: &records,
        target: &target,
    };
    let summary = prune_pass(
        &storage,
        &runner,
        &api,
        &api,
        &scope,
        &ScopedSource,
        PruneMode::Remove,
        &|_| {},
    );
    assert_eq!(summary.removed, 0, "the open same-number scope keeps it");
    assert_eq!(summary.kept_issue, 1);
    assert_eq!(summary.actions[0].reason_code, Some("issue_open"));
    assert!(path.exists());
}

#[test]
fn prune_refuses_the_whole_pass_while_a_cooperating_operation_holds_the_lock() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("prune-os-lock") else {
        return;
    };
    register(&fixture, "cli-session");
    let held = crate::worktree::operation_lock::acquire(&fixture.identity).expect("held lock");
    let summary = run(
        &fixture,
        &FakeOpenCodeApi::empty(),
        &ScriptedIssueStateSource::closed(),
    );
    assert_eq!(summary.removed, 0);
    assert_eq!(summary.kept_protected, 1);
    assert!(
        summary.actions[0]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("in progress"),
        "got: {:?}",
        summary.actions[0].message
    );
    assert!(fixture.target.exists());
    drop(held);
}

#[test]
fn an_unscoped_legacy_row_keeps_before_any_provider_read() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("prune-unscoped-no-provenance") else {
        return;
    };
    let canonical = crate::worktree::canonical_or_self(&fixture.target);
    let id = registry::stable_directory_id(&fixture.identity, &canonical.to_string_lossy());
    registry::insert_directory(
        &fixture.storage,
        &NewDirectory {
            directory_id: &id,
            repo_identity: &fixture.identity,
            checkout_path: &fixture.repo.dir.path().to_string_lossy(),
            worktree_path: &canonical.to_string_lossy(),
            branch: "phasegent/prune-target",
            created_at: 1,
            session: "cli-session",
            associations: &[NewAssociation::Unscoped { issue: 1196 }],
        },
    )
    .expect("registry row");
    let source = ScriptedIssueStateSource::closed();
    let records = registry::load_records(&fixture.storage, Some(&fixture.identity)).expect("rows");
    let target = TargetResolver::new(fixture.repo.dir.path(), &fixture.identity);
    let scope = PruneScope {
        identity: &fixture.identity,
        repo_path: fixture.repo.dir.path(),
        records: &records,
        target: &target,
    };
    let verdict = super::closure_of(&source, &scope, &records[0]);
    assert!(
        verdict
            .keep_reason()
            .unwrap_or_default()
            .contains("no corroborated"),
        "got: {verdict:?}"
    );
    assert!(
        source.asked().is_empty(),
        "an unknown row must keep before a provider read: {:?}",
        source.asked()
    );
}

/// A fixture whose database lives at an explicit path instead of the ambient
/// environment variable, so the test never publishes a temporary database
/// path to the other threads of the suite. The cache guard is declared
/// before its directory, so the ambient cache path is restored before the
/// directory is removed.
struct LocalFixture {
    repo: TempRepo,
    storage: Storage,
    identity: String,
    target: PathBuf,
    _cache_env: crate::infra::storage::test_support::EnvGuard,
    _cache: TempDir,
    _db: TempDir,
}

fn local_fixture(label: &str) -> Option<LocalFixture> {
    let repo = TempRepo::init(label)?;
    let db = TempDir::new(label);
    let storage = Storage::open_at(&db.path().join("phasegent.sqlite3")).expect("storage open");
    registry::ensure_schema(&storage).expect("registry schema");
    let cache = crate::worktree_tests::support::unique_cache(label);
    let cache_env = crate::infra::storage::test_support::EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let target = repo.dir.path().join("prune-target");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/prune-target")
        .expect("real worktree");
    Some(LocalFixture {
        repo,
        storage,
        identity,
        target,
        _cache_env: cache_env,
        _cache: cache,
        _db: db,
    })
}

fn local_register(fixture: &LocalFixture, session: &str) {
    let canonical = crate::worktree::canonical_or_self(&fixture.target);
    let id = registry::stable_directory_id(&fixture.identity, &canonical.to_string_lossy());
    registry::insert_directory(
        &fixture.storage,
        &NewDirectory {
            directory_id: &id,
            repo_identity: &fixture.identity,
            checkout_path: &fixture.repo.dir.path().to_string_lossy(),
            worktree_path: &canonical.to_string_lossy(),
            branch: "phasegent/prune-target",
            created_at: 1,
            session,
            associations: &[NewAssociation::Legacy {
                issue: 1196,
                provider: "local".to_owned(),
                project: "default".to_owned(),
                external_id: 1196,
            }],
        },
    )
    .expect("registry row");
}

fn local_run(
    fixture: &LocalFixture,
    api: &dyn OpenCodeApi,
    source: &dyn IssueStateSource,
) -> PruneSummary {
    let runner = ProcessWorktreeRunner::new();
    let records = registry::load_records(&fixture.storage, Some(&fixture.identity)).expect("rows");
    let target = TargetResolver::new(fixture.repo.dir.path(), &fixture.identity);
    let scope = PruneScope {
        identity: &fixture.identity,
        repo_path: fixture.repo.dir.path(),
        records: &records,
        target: &target,
    };
    prune_pass(
        &fixture.storage,
        &runner,
        api,
        api,
        &scope,
        source,
        PruneMode::Remove,
        &|_| {},
    )
}

/// A rejection found by the fresh re-check keeps its structured reason code
/// instead of falling back to an unnamed protected state.
#[test]
fn a_recheck_rejection_keeps_its_structured_reason_code() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = local_fixture("prune-recheck-reason") else {
        return;
    };
    local_register(&fixture, "cli-session");
    struct ClosedThenKept {
        asked: Cell<usize>,
    }
    impl IssueStateSource for ClosedThenKept {
        fn closure(&self, _repo_path: &Path, _issue: u64) -> IssueClosure {
            let asked = self.asked.get();
            self.asked.set(asked + 1);
            if asked == 0 {
                IssueClosure::Closed
            } else {
                IssueClosure::Keep("the issue reopened after the scan".to_owned())
            }
        }
    }
    let summary = local_run(
        &fixture,
        &FakeOpenCodeApi::empty(),
        &ClosedThenKept {
            asked: Cell::new(0),
        },
    );
    assert_eq!(summary.removed, 0);
    let action = &summary.actions[0];
    assert_eq!(action.result, "skipped_protected");
    assert_eq!(
        action.reason_code,
        Some("closure_provider_unavailable"),
        "the re-check keeps its structured reason code: {action:?}"
    );
    assert!(
        action
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("kept on re-check"),
        "got: {:?}",
        action.message
    );
    assert!(fixture.target.exists());
}

/// A cache-bearing candidate keeps two fresh mutation boundaries and costs
/// two check rounds; a candidate without a root `.cache` runs the
/// non-mutating probe and one final fresh check, so it stays one round cheaper.
#[test]
fn a_cached_candidate_costs_two_fresh_check_rounds_and_a_plain_one_one() {
    let _lock = lock_workflow_tests();
    let Some(plain) = local_fixture("prune-call-count-plain") else {
        return;
    };
    local_register(&plain, "cli-session");
    let plain_api = CountingApi {
        inner: FakeOpenCodeApi::empty(),
        active_calls: Cell::new(0),
        list_calls: Cell::new(0),
    };
    let plain_summary = local_run(&plain, &plain_api, &ScriptedIssueStateSource::closed());
    assert_eq!(plain_summary.removed, 1);

    let Some(cached) = local_fixture("prune-call-count-cached") else {
        return;
    };
    std::fs::create_dir_all(cached.target.join(".cache/ai")).expect("cache dir");
    std::fs::write(cached.target.join(".cache/ai/one.txt"), "scratch").expect("cache file");
    local_register(&cached, "cli-session");
    let cached_api = CountingApi {
        inner: FakeOpenCodeApi::empty(),
        active_calls: Cell::new(0),
        list_calls: Cell::new(0),
    };
    let cached_summary = local_run(&cached, &cached_api, &ScriptedIssueStateSource::closed());
    assert_eq!(cached_summary.removed, 1);

    // The flow reads the running set once per classification and once per
    // family assessment. The plain flow classifies three times (the scan, the
    // re-check and the final fresh check) and assesses the family twice (the
    // move and the final fresh check). The cached flow adds the pre-cleanup
    // check and the post-cleanup check, so it classifies four times and
    // assesses the family three times.
    assert_eq!(
        plain_api.active_calls.get(),
        5,
        "the plain flow reads the running set five times"
    );
    assert_eq!(plain_api.list_calls.get(), 2, "the plain flow lists twice");
    assert_eq!(
        cached_api.active_calls.get(),
        7,
        "the cached flow reads the running set seven times"
    );
    assert_eq!(
        cached_api.list_calls.get(),
        3,
        "the cached flow lists thrice"
    );
}

/// The measurements count a successful call and a failing one, and the
/// nested operation counts are the host calls the stub actually observed.
#[test]
fn the_timings_count_successes_failures_and_the_observed_host_calls() {
    use crate::cli::worktree::prune::timing::{TimedOpenCodeApi, TimingStats};
    let timing = TimingStats::new();
    let host = FakeOpenCodeApi::empty();
    let api = TimedOpenCodeApi::new(&host, &timing);
    assert_eq!(api.active_sessions().expect("active").len(), 0);
    assert!(
        api.session("ses_missing").is_err(),
        "the stub does not own it"
    );
    let counted = timing.measure_stage(timing::SCAN, || Ok::<usize, String>(7));
    assert_eq!(counted.expect("the stage result"), 7);
    let failed = timing.measure_stage(timing::VERIFICATION, || {
        Err::<(), String>("nope".to_owned())
    });
    assert!(failed.is_err());
    let report = timing.report();
    assert!(report.total_ms >= 0.0);
    assert_eq!(report.stages[super::super::timing::SCAN].calls, 1);
    assert_eq!(report.stages[super::super::timing::VERIFICATION].calls, 1);
    assert_eq!(
        report.operations[super::super::timing::HOST_ACTIVE].calls,
        1,
        "the successful host call is counted"
    );
    assert_eq!(
        report.operations[super::super::timing::HOST_GET].calls,
        1,
        "the failing host call is counted too"
    );
    assert_eq!(report.operations[super::super::timing::HOST_LIST].calls, 0);
    let serialized = serde_json::to_string(&report).expect("report");
    assert!(
        !serialized.contains("ses_"),
        "a measured report carries no session identity: {serialized}"
    );
}

/// The ordered log of the boundary calls one removal made.
#[derive(Default)]
struct EventLog {
    events: RefCell<Vec<String>>,
}

impl EventLog {
    fn record(&self, event: &str) {
        self.events.borrow_mut().push(event.to_owned());
    }

    /// Whether the last recorded event was a host call.
    fn last_was_host(&self) -> bool {
        self.events
            .borrow()
            .last()
            .is_some_and(|event| event.starts_with("host:"))
    }

    fn snapshot(&self) -> Vec<String> {
        self.events.borrow().clone()
    }
}

/// The Git boundary with its calls recorded, and the bounded faults a test
/// injects through it: a change while the removal-stage probe runs, a change
/// while the worktree removal runs, and a failing removal.
struct RecordingRunner<'a> {
    inner: ProcessWorktreeRunner,
    log: &'a EventLog,
    fired: Cell<bool>,
    during_probe: Option<&'a (dyn Fn() + 'a)>,
    during_removal: Option<&'a (dyn Fn() + 'a)>,
    fail_removal: bool,
}

impl<'a> RecordingRunner<'a> {
    fn new(log: &'a EventLog, during_probe: Option<&'a (dyn Fn() + 'a)>) -> Self {
        Self {
            inner: ProcessWorktreeRunner::new(),
            log,
            fired: Cell::new(false),
            during_probe,
            during_removal: None,
            fail_removal: false,
        }
    }

    fn with_removal_change(mut self, during_removal: &'a (dyn Fn() + 'a)) -> Self {
        self.during_removal = Some(during_removal);
        self
    }

    fn with_failing_removal(mut self) -> Self {
        self.fail_removal = true;
        self
    }
}

impl WorktreeRunner for RecordingRunner<'_> {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError> {
        // The removal-stage probe is the first Git call that follows a host
        // call: the family move has already happened, so nothing but the
        // final fresh check may follow it.
        if args.first() == Some(&"status")
            && !self.fired.get()
            && self.log.last_was_host()
            && let Some(change) = self.during_probe
        {
            self.fired.set(true);
            change();
        }
        self.log.record(&format!("git:{}", args.join(" ")));
        if args.first() == Some(&"worktree") {
            if let Some(change) = self.during_removal {
                change();
            }
            if self.fail_removal {
                return Ok(GitOutput {
                    status: 1,
                    stdout: String::new(),
                });
            }
        }
        self.inner.run(args, workdir)
    }
}

/// The host boundary with its calls recorded, and the optional flag it sets
/// when the family relocation reaches the host listing.
struct RecordingApi<'a> {
    inner: &'a dyn OpenCodeApi,
    log: &'a EventLog,
    on_list: Option<&'a Cell<bool>>,
}

impl OpenCodeApi for RecordingApi<'_> {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.log.record("host:get");
        self.inner.session(id)
    }
    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.log.record("host:list");
        // The first listing is the family assessment of the removal stage.
        if let Some(flag) = self.on_list {
            flag.set(true);
        }
        self.inner.list(request)
    }
    fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
        self.log.record("host:move");
        self.inner.move_session(id, directory)
    }
    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.log.record("host:active");
        self.inner.active_sessions()
    }
}

/// The closure source with its evaluations recorded, and the optional
/// protection change it reports once the family relocation has happened.
struct RecordingSource<'a> {
    inner: &'a dyn IssueStateSource,
    log: &'a EventLog,
    reopened: Option<&'a Cell<bool>>,
}

impl RecordingSource<'_> {
    fn verdict(&self, verdict: IssueClosure) -> IssueClosure {
        match self.reopened {
            Some(flag) if flag.get() => {
                IssueClosure::Open("the issue reopened during the family relocation".to_owned())
            }
            _ => verdict,
        }
    }
}

impl IssueStateSource for RecordingSource<'_> {
    fn closure(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        self.log.record("closure");
        self.verdict(self.inner.closure(repo_path, issue))
    }
    fn own_task_closure(
        &self,
        repo_path: &Path,
        task: crate::cli::issue_closure::TaskScopeRef<'_>,
        issue: u64,
    ) -> IssueClosure {
        self.log.record("closure");
        self.verdict(self.inner.own_task_closure(repo_path, task, issue))
    }
    fn associated_closure(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        self.log.record("closure");
        self.verdict(self.inner.associated_closure(repo_path, issue))
    }
    fn scoped_closure(
        &self,
        repo_path: &Path,
        issue: u64,
        scope: Option<&crate::worktree::WorktreeScope>,
    ) -> IssueClosure {
        self.log.record("closure");
        self.verdict(self.inner.scoped_closure(repo_path, issue, scope))
    }
}

/// Run one removal with every boundary recorded.
fn recorded_run(
    fixture: &LocalFixture,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    source: &dyn IssueStateSource,
) -> PruneSummary {
    let records = registry::load_records(&fixture.storage, Some(&fixture.identity)).expect("rows");
    let target = TargetResolver::new(fixture.repo.dir.path(), &fixture.identity);
    let scope = PruneScope {
        identity: &fixture.identity,
        repo_path: fixture.repo.dir.path(),
        records: &records,
        target: &target,
    };
    prune_pass(
        &fixture.storage,
        runner,
        api,
        api,
        &scope,
        source,
        PruneMode::Remove,
        &|_| {},
    )
}

/// The removal-stage probe runs before the last fresh check, so no external
/// Git process sits between that check and the removal.
#[test]
fn the_removal_probe_runs_before_the_last_fresh_check() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = local_fixture("prune-probe-order") else {
        return;
    };
    local_register(&fixture, "cli-session");
    let log = EventLog::default();
    let runner = RecordingRunner::new(&log, None);
    let host = FakeOpenCodeApi::empty();
    let api = RecordingApi {
        inner: &host,
        log: &log,
        on_list: None,
    };
    let closure = ScriptedIssueStateSource::closed();
    let source = RecordingSource {
        inner: &closure,
        log: &log,
        reopened: None,
    };
    let summary = recorded_run(&fixture, &runner, &api, &source);
    assert_eq!(summary.removed, 1, "the removal happened: {summary:?}");
    let events = log.snapshot();
    let removal = events
        .iter()
        .position(|event| event.starts_with("git:worktree remove"))
        .expect("the removal ran");
    let last_fresh = events[..removal]
        .iter()
        .rposition(|event| event.starts_with("host:") || event.starts_with("closure:"))
        .expect("a fresh check ran");
    assert!(
        !events[last_fresh + 1..removal]
            .iter()
            .any(|event| event.starts_with("git:")),
        "no external Git process runs between the final fresh check and the removal: {events:?}"
    );
    let probe = events[..removal]
        .iter()
        .rposition(|event| event == "git:status --porcelain")
        .expect("a cleanliness probe ran");
    assert!(
        probe < last_fresh,
        "the removal-stage probe runs before the final fresh check: {events:?}"
    );
}

/// A protection that changes while the removal-stage probe runs is still
/// seen by the final fresh check: the directory survives.
#[test]
fn a_registration_that_disappears_during_the_probe_keeps_the_directory() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = local_fixture("prune-probe-mutation") else {
        return;
    };
    local_register(&fixture, "cli-session");
    let canonical = crate::worktree::canonical_or_self(&fixture.target);
    let directory_id =
        registry::stable_directory_id(&fixture.identity, &canonical.to_string_lossy());
    let storage = &fixture.storage;
    let identity = fixture.identity.clone();
    let log = EventLog::default();
    let change = move || {
        registry::remove_directory(storage, &identity, &directory_id)
            .expect("the registration is removed");
    };
    let runner = RecordingRunner::new(&log, Some(&change));
    let host = FakeOpenCodeApi::empty();
    let api = RecordingApi {
        inner: &host,
        log: &log,
        on_list: None,
    };
    let closure = ScriptedIssueStateSource::closed();
    let source = RecordingSource {
        inner: &closure,
        log: &log,
        reopened: None,
    };
    let summary = recorded_run(&fixture, &runner, &api, &source);
    assert_eq!(summary.removed, 0, "the directory is kept: {summary:?}");
    assert_eq!(summary.kept_protected, 1);
    assert_eq!(
        summary.actions[0].reason_code,
        Some("registry_record_missing"),
        "the final fresh check sees the change: {:?}",
        summary.actions[0]
    );
    assert!(fixture.target.exists(), "the worktree survives the change");
}

/// The reclamation stage times the removal itself: the cache clearance or its
/// cleanliness probe, the Git worktree removal and the registry row removal.
#[test]
fn the_reclamation_stage_measures_the_git_and_registry_removals() {
    use crate::cli::worktree::prune::timing::{
        CACHE_DELETE_RECLAIM, TimedIssueStateSource, TimedOpenCodeApi, TimingStats, VERIFICATION,
    };
    let _lock = lock_workflow_tests();
    let Some(fixture) = local_fixture("prune-reclamation-stage") else {
        return;
    };
    local_register(&fixture, "cli-session");
    let timing = TimingStats::new();
    let host = FakeOpenCodeApi::empty();
    let api = TimedOpenCodeApi::new(&host, &timing);
    let closure = ScriptedIssueStateSource::closed();
    let source = TimedIssueStateSource::new(&closure, &timing);
    let records = registry::load_records(&fixture.storage, Some(&fixture.identity)).expect("rows");
    let target = TargetResolver::new(fixture.repo.dir.path(), &fixture.identity);
    let scope = PruneScope {
        identity: &fixture.identity,
        repo_path: fixture.repo.dir.path(),
        records: &records,
        target: &target,
    };
    let summary = prune_pass_with_snapshot_timed(
        &fixture.storage,
        &ProcessWorktreeRunner::new(),
        &api,
        &api,
        &scope,
        &source,
        PruneMode::Remove,
        MovePolicy::NoWait,
        &|_| {},
        &std::cell::OnceCell::new(),
        false,
        &timing,
    );
    assert_eq!(summary.removed, 1);
    let report = timing.report();
    assert_eq!(
        report.stages[CACHE_DELETE_RECLAIM].calls, 3,
        "the cleanliness probe, the git removal and the registry removal: {report:?}"
    );
    assert_eq!(
        report.stages[VERIFICATION].calls, 2,
        "the re-check and the final fresh check: {report:?}"
    );
}

/// A cache-bearing candidate keeps a fresh check on each side of the cache
/// deletion: one after the family relocation and before the deletion, and one
/// after the deletion and before the removal.
#[test]
fn the_cached_removal_checks_around_the_cache_deletion() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = local_fixture("prune-cached-order") else {
        return;
    };
    std::fs::create_dir_all(fixture.target.join(".cache/ai")).expect("cache dir");
    std::fs::write(fixture.target.join(".cache/ai/one.txt"), "scratch").expect("cache file");
    local_register(&fixture, "cli-session");
    let log = EventLog::default();
    let runner = RecordingRunner::new(&log, None);
    let host = FakeOpenCodeApi::empty();
    let api = RecordingApi {
        inner: &host,
        log: &log,
        on_list: None,
    };
    let closure = ScriptedIssueStateSource::closed();
    let source = RecordingSource {
        inner: &closure,
        log: &log,
        reopened: None,
    };
    let summary = recorded_run(&fixture, &runner, &api, &source);
    assert_eq!(summary.removed, 1, "the removal happened: {summary:?}");
    let events = log.snapshot();
    let move_index = events
        .iter()
        .position(|event| event == "host:list")
        .expect("the family relocation listed the host");
    let removal = events
        .iter()
        .position(|event| event.starts_with("git:worktree remove"))
        .expect("the removal ran");
    // The cache deletion inspects the cache last: the fresh check after the
    // deletion finds no root .cache left and inspects nothing.
    let clearance = events[..removal]
        .iter()
        .rposition(|event| event == "git:ls-files -- .cache")
        .expect("the cache was inspected");
    let fresh_between = events[move_index..clearance]
        .iter()
        .any(|event| event.starts_with("host:") || event.starts_with("closure:"));
    assert!(
        fresh_between,
        "a fresh check runs between the family relocation and the cache deletion: {events:?}"
    );
    let fresh_after = events[clearance..removal]
        .iter()
        .any(|event| event.starts_with("host:") || event.starts_with("closure:"));
    assert!(
        fresh_after,
        "a fresh check runs between the cache deletion and the removal: {events:?}"
    );
    let last_fresh = events[..removal]
        .iter()
        .rposition(|event| event.starts_with("host:") || event.starts_with("closure:"))
        .expect("a fresh check ran");
    assert!(
        !events[last_fresh + 1..removal]
            .iter()
            .any(|event| event.starts_with("git:")),
        "no external Git process runs between the final fresh check and the removal: {events:?}"
    );
}

/// A protection that changes while the family relocation runs is seen before
/// the cache is deleted: the cache stays untouched and the directory stays.
#[test]
fn a_protection_change_during_the_family_relocation_keeps_the_cache() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = local_fixture("prune-family-change") else {
        return;
    };
    std::fs::create_dir_all(fixture.target.join(".cache/ai")).expect("cache dir");
    std::fs::write(fixture.target.join(".cache/ai/one.txt"), "scratch").expect("cache file");
    local_register(&fixture, "cli-session");
    let log = EventLog::default();
    let runner = RecordingRunner::new(&log, None);
    let host = FakeOpenCodeApi::empty();
    let relocated = Cell::new(false);
    let api = RecordingApi {
        inner: &host,
        log: &log,
        on_list: Some(&relocated),
    };
    let closure = ScriptedIssueStateSource::closed();
    let source = RecordingSource {
        inner: &closure,
        log: &log,
        reopened: Some(&relocated),
    };
    let summary = recorded_run(&fixture, &runner, &api, &source);
    assert_eq!(summary.removed, 0, "the directory is kept: {summary:?}");
    assert_eq!(summary.kept_protected, 1);
    assert_eq!(
        summary.actions[0].reason_code,
        Some("issue_open"),
        "the fresh check after the relocation sees the change: {:?}",
        summary.actions[0]
    );
    assert!(
        fixture.target.join(".cache/ai/one.txt").exists(),
        "the cache was not deleted"
    );
    assert!(fixture.target.exists(), "the worktree survives");
}

/// A failing worktree removal is counted and timed in the reclamation stage.
#[test]
fn a_failed_worktree_removal_is_timed_in_the_reclamation_stage() {
    use crate::cli::worktree::prune::timing::{
        CACHE_DELETE_RECLAIM, TimedIssueStateSource, TimedOpenCodeApi, TimingStats,
    };
    let _lock = lock_workflow_tests();
    let Some(fixture) = local_fixture("prune-remove-failure") else {
        return;
    };
    local_register(&fixture, "cli-session");
    let log = EventLog::default();
    let runner = RecordingRunner::new(&log, None).with_failing_removal();
    let timing = TimingStats::new();
    let host = FakeOpenCodeApi::empty();
    let api = TimedOpenCodeApi::new(&host, &timing);
    let closure = ScriptedIssueStateSource::closed();
    let source = TimedIssueStateSource::new(&closure, &timing);
    let records = registry::load_records(&fixture.storage, Some(&fixture.identity)).expect("rows");
    let target = TargetResolver::new(fixture.repo.dir.path(), &fixture.identity);
    let scope = PruneScope {
        identity: &fixture.identity,
        repo_path: fixture.repo.dir.path(),
        records: &records,
        target: &target,
    };
    let summary = prune_pass_with_snapshot_timed(
        &fixture.storage,
        &runner,
        &api,
        &api,
        &scope,
        &source,
        PruneMode::Remove,
        MovePolicy::NoWait,
        &|_| {},
        &std::cell::OnceCell::new(),
        false,
        &timing,
    );
    assert_eq!(summary.failed, 1, "the removal failed: {summary:?}");
    assert_eq!(
        summary.actions[0].reason_code,
        Some("worktree_remove_failed"),
        "got: {:?}",
        summary.actions[0]
    );
    let report = timing.report();
    assert_eq!(
        report.stages[CACHE_DELETE_RECLAIM].calls, 2,
        "the probe and the failed removal are counted: {report:?}"
    );
    assert!(
        fixture.target.exists(),
        "the worktree survives a failed removal"
    );
}

/// A failing registry cleanup is counted and timed in the reclamation stage.
#[test]
fn a_failed_registry_cleanup_is_timed_in_the_reclamation_stage() {
    use crate::cli::worktree::prune::timing::{
        CACHE_DELETE_RECLAIM, TimedIssueStateSource, TimedOpenCodeApi, TimingStats,
    };
    let _lock = lock_workflow_tests();
    let Some(fixture) = local_fixture("prune-registry-failure") else {
        return;
    };
    local_register(&fixture, "cli-session");
    let log = EventLog::default();
    // The registry table disappears while the worktree is removed, so the
    // registry cleanup that follows it fails.
    let storage = &fixture.storage;
    let break_registry = || {
        storage
            .connection
            .execute("DROP TABLE worktree_directories", [])
            .expect("the registry table is dropped");
    };
    let runner = RecordingRunner::new(&log, None).with_removal_change(&break_registry);
    let timing = TimingStats::new();
    let host = FakeOpenCodeApi::empty();
    let api = TimedOpenCodeApi::new(&host, &timing);
    let closure = ScriptedIssueStateSource::closed();
    let source = TimedIssueStateSource::new(&closure, &timing);
    let records = registry::load_records(&fixture.storage, Some(&fixture.identity)).expect("rows");
    let target = TargetResolver::new(fixture.repo.dir.path(), &fixture.identity);
    let scope = PruneScope {
        identity: &fixture.identity,
        repo_path: fixture.repo.dir.path(),
        records: &records,
        target: &target,
    };
    let summary = prune_pass_with_snapshot_timed(
        &fixture.storage,
        &runner,
        &api,
        &api,
        &scope,
        &source,
        PruneMode::Remove,
        MovePolicy::NoWait,
        &|_| {},
        &std::cell::OnceCell::new(),
        false,
        &timing,
    );
    assert_eq!(summary.failed, 1, "the cleanup failed: {summary:?}");
    assert_eq!(
        summary.actions[0].reason_code,
        Some("registry_cleanup_failed"),
        "got: {:?}",
        summary.actions[0]
    );
    let report = timing.report();
    assert_eq!(
        report.stages[CACHE_DELETE_RECLAIM].calls, 3,
        "the probe, the removal and the failed cleanup are counted: {report:?}"
    );
}
