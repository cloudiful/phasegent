use super::support::*;
use super::*;

struct Fixture {
    repo: TempRepo,
    _db: TempDir,
    _db_env: EnvGuard,
    cache: TempDir,
    _cache_env: EnvGuard,
    identity: String,
}

fn fixture(label: &str) -> Option<Fixture> {
    let repo = TempRepo::init(label)?;
    let (db, db_env) = open_temp_db(label);
    let cache = temp_cache(label);
    let cache_env = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    Some(Fixture {
        repo,
        _db: db,
        _db_env: db_env,
        cache,
        _cache_env: cache_env,
        identity,
    })
}

#[test]
fn a_free_checkout_is_reused_and_recorded_for_the_session() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-reuse") else {
        return;
    };
    let outcome = acquire(&fixture.repo, &fixture.cache, 245, "session-A", false).expect("acquire");
    assert_eq!(outcome.reason, "no_conflict");
    assert!(!outcome.created);
    assert_eq!(
        outcome.path,
        fixture.repo.dir.path().to_string_lossy(),
        "a free checkout is reused in place"
    );
    assert_eq!(outcome.branch, fixture.repo.head_branch);
    assert!(
        !fixture
            .cache
            .path()
            .join("worktrees")
            .join("worktrees")
            .exists(),
        "reuse creates no per-worktree directory"
    );
    let entries: Vec<_> = std::fs::read_dir(fixture.cache.path().join("worktrees"))
        .expect("lock directory")
        .collect();
    assert!(
        entries.iter().all(|entry| entry
            .as_ref()
            .map(|entry| entry.file_name().to_string_lossy().ends_with(".lock"))
            .unwrap_or(false)),
        "reuse leaves only the operation lock, never a worktree directory: {entries:?}"
    );
    let records = open_records(&fixture.identity);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].issue_numbers(), vec![245]);
    assert_eq!(records[0].sessions, vec!["session-A"]);
}

fn acquire_at(
    fixture: &Fixture,
    path: &std::path::Path,
    issue: u64,
    session: &str,
) -> Result<crate::worktree::AcquireOutcome, crate::worktree::WorktreeError> {
    crate::worktree::acquire_directory_with_foreign_state(
        &ProcessWorktreeRunner::new(),
        path,
        issue,
        session,
        crate::worktree::AcquireOptions {
            cache_base: Some(fixture.cache.path()),
            isolate: false,
            base: None,
            reuse: false,
        },
        None,
        None,
    )
}

#[test]
fn acquiring_from_a_main_subdirectory_registers_the_checkout_root() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-main-nested") else {
        return;
    };
    let nested = fixture.repo.dir.path().join("nested/homelab");
    std::fs::create_dir_all(&nested).expect("nested directory");
    let outcome = acquire_at(&fixture, &nested, 245, "session-A").expect("acquire from nested");
    let root = crate::worktree::canonical_or_self(fixture.repo.dir.path());
    assert_eq!(outcome.path, root.to_string_lossy());
    let records = open_records(&fixture.identity);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].worktree_path, root.to_string_lossy());

    let repeat = acquire_at(&fixture, fixture.repo.dir.path(), 245, "session-A")
        .expect("acquire from the root");
    assert_eq!(repeat.reason, "idempotent");
    assert_eq!(repeat.directory_id, outcome.directory_id);
    assert_eq!(
        open_records(&fixture.identity).len(),
        1,
        "the nested acquisition must not add a second registration"
    );
}

#[test]
fn acquiring_from_a_trailing_space_checkout_registers_the_exact_root() {
    let _lock = lock_workflow_tests();
    let base = TempDir::new("acquire-spaced");
    let runner = ProcessWorktreeRunner::new();
    let root = base.path().join("repo ");
    let sibling = base.path().join("repo");
    for path in [&root, &sibling] {
        std::fs::create_dir_all(path).expect("repository directory");
        assert!(
            runner
                .run(&["init", "-q", "-b", "main"], path)
                .map(|output| output.status == 0)
                .unwrap_or(false)
        );
        let _ = runner.run(
            &[
                "-c",
                "user.name=phasegent-test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
            path,
        );
    }
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).expect("nested directory");
    let (_db, _db_env) = open_temp_db("acquire-spaced");
    let cache = temp_cache("acquire-spaced");
    let _cache_env = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    let identity = repo_identity(&runner, &root).expect("spaced identity");
    let outcome = crate::worktree::acquire_directory_with_foreign_state(
        &runner,
        &nested,
        245,
        "session-A",
        crate::worktree::AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: false,
            base: None,
            reuse: false,
        },
        None,
        None,
    )
    .expect("acquire from the spaced checkout");
    let expected = root.canonicalize().expect("canonical spaced root");
    assert_eq!(outcome.path, expected.to_string_lossy());
    let records = open_records(&identity);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].worktree_path, expected.to_string_lossy());
}

#[cfg(unix)]
#[test]
fn acquiring_from_a_trailing_cr_checkout_registers_the_exact_root() {
    let _lock = lock_workflow_tests();
    let base = TempDir::new("acquire-cr");
    let runner = ProcessWorktreeRunner::new();
    let root = base.path().join("repo\r");
    let sibling = base.path().join("repo");
    for path in [&root, &sibling] {
        std::fs::create_dir_all(path).expect("repository directory");
        assert!(
            runner
                .run(&["init", "-q", "-b", "main"], path)
                .map(|output| output.status == 0)
                .unwrap_or(false)
        );
        let _ = runner.run(
            &[
                "-c",
                "user.name=phasegent-test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
            path,
        );
    }
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).expect("nested directory");
    let (_db, _db_env) = open_temp_db("acquire-cr");
    let cache = temp_cache("acquire-cr");
    let _cache_env = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    let identity = repo_identity(&runner, &root).expect("CR identity");
    let outcome = crate::worktree::acquire_directory_with_foreign_state(
        &runner,
        &nested,
        245,
        "session-A",
        crate::worktree::AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: false,
            base: None,
            reuse: false,
        },
        None,
        None,
    )
    .expect("acquire from the CR checkout");
    let expected = root.canonicalize().expect("canonical CR root");
    assert_eq!(outcome.path, expected.to_string_lossy());
    let records = open_records(&identity);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].worktree_path, expected.to_string_lossy());
}

#[test]
fn acquiring_root_first_then_from_a_subdirectory_is_idempotent() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-root-first") else {
        return;
    };
    let first = acquire_at(&fixture, fixture.repo.dir.path(), 245, "session-A")
        .expect("acquire from the root");
    let nested = fixture.repo.dir.path().join("nested");
    std::fs::create_dir_all(&nested).expect("nested directory");
    let second =
        acquire_at(&fixture, &nested, 245, "session-A").expect("acquire from the nested path");
    let root = crate::worktree::canonical_or_self(fixture.repo.dir.path());
    assert_eq!(second.reason, "idempotent");
    assert_eq!(second.directory_id, first.directory_id);
    assert_eq!(second.path, root.to_string_lossy());
    let records = open_records(&fixture.identity);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].worktree_path, root.to_string_lossy());
}

#[test]
fn acquiring_from_a_linked_subdirectory_registers_the_linked_root() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-linked-nested") else {
        return;
    };
    let linked = fixture.repo.dir.path().join("linked");
    crate::worktree::worktree_add(
        &ProcessWorktreeRunner::new(),
        fixture.repo.dir.path(),
        &linked,
        "phasegent/issue37-linked",
    )
    .expect("linked worktree");
    let nested = linked.join("nested");
    std::fs::create_dir_all(&nested).expect("nested directory");
    let outcome = acquire_at(&fixture, &nested, 245, "session-A").expect("acquire from nested");
    let root = crate::worktree::canonical_or_self(&linked);
    assert_eq!(outcome.path, root.to_string_lossy());
    let records = open_records(&fixture.identity);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].worktree_path, root.to_string_lossy());

    let repeat = acquire_at(&fixture, &linked, 245, "session-A").expect("acquire from the root");
    assert_eq!(repeat.reason, "idempotent");
    assert_eq!(repeat.directory_id, outcome.directory_id);
    assert_eq!(open_records(&fixture.identity).len(), 1);
}

#[test]
fn explicit_isolation_creates_a_registered_worktree_under_the_cache() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-isolate") else {
        return;
    };
    let outcome = acquire(&fixture.repo, &fixture.cache, 245, "session-A", true).expect("acquire");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome.branch.starts_with("phasegent/245-"),
        "isolation uses the issue branch slug"
    );
    assert!(
        outcome
            .path
            .starts_with(&fixture.cache.path().to_string_lossy().to_string()),
        "the worktree lands under the operation cache"
    );
    assert!(std::path::Path::new(&outcome.path).exists());
    let records = open_records(&fixture.identity);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].worktree_path, outcome.path);
    assert_eq!(records[0].issue_numbers(), vec![245]);
}

#[test]
fn the_same_session_and_issue_returns_the_registered_directory() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-idempotent") else {
        return;
    };
    let first = acquire(&fixture.repo, &fixture.cache, 245, "session-A", true).expect("first");
    let second = acquire(&fixture.repo, &fixture.cache, 245, "session-A", true).expect("second");
    assert_eq!(second.reason, "idempotent");
    assert_eq!(second.directory_id, first.directory_id);
    assert_eq!(second.path, first.path);
    assert!(!second.created);
    assert_eq!(open_records(&fixture.identity).len(), 1);
}

#[test]
fn a_second_session_refuses_the_occupied_checkout_without_explicit_isolation() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-second-session") else {
        return;
    };
    let first = acquire(&fixture.repo, &fixture.cache, 245, "session-A", false).expect("first");
    let error = acquire(&fixture.repo, &fixture.cache, 246, "session-B", false)
        .expect_err("an occupied checkout must not be auto-isolated");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("--isolate"),
        "got: {}",
        error.message
    );
    let records = open_records(&fixture.identity);
    assert_eq!(
        records.len(),
        1,
        "no worktree was created without the opt-in"
    );
    assert_eq!(records[0].worktree_path, first.path);
}

#[test]
fn a_blank_session_is_rejected_before_any_registration() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-blank") else {
        return;
    };
    let error = acquire(&fixture.repo, &fixture.cache, 245, "   ", false)
        .expect_err("blank session must fail");
    assert_eq!(error.kind, "argument");
    assert!(open_records(&fixture.identity).is_empty());
}

struct ClosedIssues(std::collections::BTreeSet<u64>);

impl crate::worktree::occupancy::ForeignIssueState for ClosedIssues {
    fn task_closed(
        &self,
        _repo_path: &std::path::Path,
        task: &crate::worktree::DirectoryTask,
    ) -> bool {
        self.0.contains(&task.issue)
    }

    fn legacy_closed(
        &self,
        _repo_path: &std::path::Path,
        issue: &crate::worktree::DirectoryLegacyIssue,
    ) -> bool {
        self.0.contains(&issue.issue)
    }
}

fn acquire_with_closure(
    fixture: &Fixture,
    issue: u64,
    session: &str,
    closed: &[u64],
) -> Result<crate::worktree::AcquireOutcome, crate::worktree::WorktreeError> {
    let state = ClosedIssues(closed.iter().copied().collect());
    crate::worktree::acquire_directory_with_foreign_state(
        &ProcessWorktreeRunner::new(),
        fixture.repo.dir.path(),
        issue,
        session,
        crate::worktree::AcquireOptions {
            cache_base: Some(fixture.cache.path()),
            isolate: false,
            base: None,
            reuse: false,
        },
        Some(&state),
        None,
    )
}

#[test]
fn a_confirmed_closed_registration_does_not_veto_reuse() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-closed-reuse") else {
        return;
    };
    acquire(&fixture.repo, &fixture.cache, 245, "session-A", false).expect("first");
    let second =
        acquire_with_closure(&fixture, 246, "session-B", &[245]).expect("closed history reuses");
    assert!(!second.created);
    assert_eq!(
        second.path,
        fixture.repo.dir.path().to_string_lossy(),
        "the confirmed closed historical task no longer owns the checkout"
    );
    let records = open_records(&fixture.identity);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].issue_numbers(), vec![245, 246]);
}

#[test]
fn an_open_registration_refuses_with_explicit_isolation_guidance() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-open-protect") else {
        return;
    };
    acquire(&fixture.repo, &fixture.cache, 245, "session-A", false).expect("first");
    let error = acquire_with_closure(&fixture, 246, "session-B", &[])
        .expect_err("an open foreign task must surface the conflict");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("--isolate"),
        "got: {}",
        error.message
    );
    assert_eq!(
        open_records(&fixture.identity).len(),
        1,
        "no directory was created automatically"
    );
}

#[test]
fn a_held_repository_operation_lock_refuses_a_second_acquire() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-lock") else {
        return;
    };
    let held = crate::worktree::operation_lock::acquire(&fixture.identity).expect("lock");
    let error = acquire(&fixture.repo, &fixture.cache, 245, "session-A", false)
        .expect_err("a cooperating operation must wait");
    assert_eq!(error.kind, "busy");
    drop(held);
    acquire(&fixture.repo, &fixture.cache, 245, "session-A", false).expect("retry succeeds");
}

#[test]
fn a_foreign_host_occupant_conflicts_before_the_checkout_is_registered() {
    use crate::worktree::opencode::test_support::FakeOpenCodeApi;

    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-unregistered-host-occupant") else {
        return;
    };
    let api = FakeOpenCodeApi::empty().with_session("ses_live", fixture.repo.dir.path());
    let error = acquire_custom(&fixture, 246, "session-B", false, None, &[], Some(&api))
        .expect_err("a foreign host occupant must protect an unregistered checkout");
    assert_eq!(error.kind, "isolation");
    assert!(open_records(&fixture.identity).is_empty());
}

#[test]
fn a_fresh_foreign_host_occupant_conflicts_even_when_the_task_is_closed() {
    use crate::worktree::opencode::test_support::FakeOpenCodeApi;

    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-host-occupant") else {
        return;
    };
    acquire(&fixture.repo, &fixture.cache, 245, "session-A", false).expect("first");
    let api = FakeOpenCodeApi::empty().with_session("ses_live", fixture.repo.dir.path());
    let state = ClosedIssues([245].into_iter().collect());
    let error = crate::worktree::acquire_directory_with_foreign_state(
        &ProcessWorktreeRunner::new(),
        fixture.repo.dir.path(),
        246,
        "session-B",
        crate::worktree::AcquireOptions {
            cache_base: Some(fixture.cache.path()),
            isolate: false,
            base: None,
            reuse: false,
        },
        Some(&state),
        Some(&api),
    )
    .expect_err("a fresh host occupant must keep the checkout");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("currently occupies"),
        "got: {}",
        error.message
    );
}

type AcquireResult = Result<crate::worktree::AcquireOutcome, crate::worktree::WorktreeError>;

#[allow(clippy::too_many_arguments)]
fn acquire_custom(
    fixture: &Fixture,
    issue: u64,
    session: &str,
    isolate: bool,
    base: Option<&str>,
    closed: &[u64],
    host: Option<&dyn crate::worktree::opencode::OpenCodeApi>,
) -> AcquireResult {
    let state = ClosedIssues(closed.iter().copied().collect());
    crate::worktree::acquire_directory_with_foreign_state(
        &ProcessWorktreeRunner::new(),
        fixture.repo.dir.path(),
        issue,
        session,
        crate::worktree::AcquireOptions {
            cache_base: Some(fixture.cache.path()),
            isolate,
            base,
            reuse: false,
        },
        Some(&state),
        host,
    )
}

#[test]
fn an_idempotent_repeat_checks_a_foreign_association_before_returning() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-idempotent-foreign") else {
        return;
    };
    let first = acquire(&fixture.repo, &fixture.cache, 245, "session-A", false).expect("first");
    let records = open_records(&fixture.identity);
    crate::worktree::registry::attach_association(
        &Storage::open().expect("storage"),
        &records[0].directory_id,
        &fixture.identity,
        2,
        "session-A",
        &crate::worktree::registry::NewAssociation::Unscoped { issue: 246 },
    )
    .expect("foreign association");
    let error = acquire(&fixture.repo, &fixture.cache, 245, "session-A", false)
        .expect_err("an exact own association must not hide the rest of the directory");
    assert_eq!(error.kind, "isolation");
    assert_eq!(open_records(&fixture.identity)[0].worktree_path, first.path);
}

#[test]
fn an_idempotent_repeat_checks_fresh_host_occupancy_before_returning() {
    use crate::worktree::opencode::test_support::FakeOpenCodeApi;

    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-idempotent-host") else {
        return;
    };
    acquire(&fixture.repo, &fixture.cache, 245, "session-A", false).expect("first");
    let api = FakeOpenCodeApi::empty().with_session("ses_foreign", fixture.repo.dir.path());
    let error = acquire_custom(&fixture, 245, "session-A", false, None, &[], Some(&api))
        .expect_err("a fresh host occupant must block even the idempotent path");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("currently occupies"),
        "got: {}",
        error.message
    );
}

#[test]
fn explicit_isolation_creates_a_separate_tree_from_an_occupied_original() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-isolate-occupied") else {
        return;
    };
    let first = acquire(&fixture.repo, &fixture.cache, 245, "session-A", false).expect("first");
    let isolated = acquire_custom(&fixture, 246, "session-B", true, None, &[], None)
        .expect("explicit --isolate must bypass the occupied original checkout");
    assert!(isolated.created);
    assert_ne!(isolated.path, first.path);
    assert_ne!(isolated.path, fixture.repo.dir.path().to_string_lossy());
    assert!(std::path::Path::new(&isolated.path).exists());
    assert_eq!(open_records(&fixture.identity).len(), 2);
}

#[test]
fn a_base_ref_without_explicit_isolation_never_creates_a_worktree() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-base-alone") else {
        return;
    };
    let error = acquire_custom(&fixture, 245, "session-A", false, Some("HEAD"), &[], None)
        .expect_err("--base alone must not create");
    assert_eq!(error.kind, "argument");
    assert!(
        error.message.contains("--isolate"),
        "got: {}",
        error.message
    );
    assert!(open_records(&fixture.identity).is_empty());
}

#[test]
fn an_externally_removed_isolated_target_is_never_returned_as_success() {
    let _lock = lock_workflow_tests();
    let Some(fixture) = fixture("acquire-removed-target") else {
        return;
    };
    let first = acquire(&fixture.repo, &fixture.cache, 245, "session-A", true).expect("isolated");
    crate::worktree::git::worktree_remove(
        &ProcessWorktreeRunner::new(),
        fixture.repo.dir.path(),
        std::path::Path::new(&first.path),
    )
    .expect("external removal");
    assert!(!std::path::Path::new(&first.path).exists());

    for isolate in [false, true] {
        let error = acquire_custom(&fixture, 245, "session-A", isolate, None, &[], None)
            .expect_err("a missing registered worktree is never a successful placement");
        assert_eq!(error.kind, "state");
        // The runtime error is bounded to a fixed character budget. A long
        // scratch path can consume that budget before the trailing "is missing"
        // wording, so assert the stable diagnostic prefix rather than a tail
        // fragment whose visibility depends on the fixture path length.
        assert!(
            error.message.contains("registered worktree"),
            "got: {}",
            error.message
        );
    }
    assert_eq!(open_records(&fixture.identity).len(), 1);
}
