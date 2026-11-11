//! Integration protection for the prune removal stage's bounded mutation
//! windows: cache clearance cannot cache an absence, a reopened issue, or a
//! newly arrived foreign occupant.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use super::*;
use crate::cli::issue_closure::test_support::ScriptedIssueStateSource;
use crate::cli::issue_closure::{IssueClosure, IssueStateSource};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::worktree::opencode::test_support::FakeOpenCodeApi;
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};
use crate::worktree::registry::{self, NewAssociation, NewDirectory};
use crate::worktree::{ProcessWorktreeRunner, repo_identity, worktree_add};
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
                Some("redmine") => IssueClosure::Keep("the redmine scope is still open".to_owned()),
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
