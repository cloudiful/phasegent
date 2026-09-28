use super::support::*;
use super::*;

use crate::worktree::{AcquireOptions, acquire_lease_with};

/// Seed one active lease row for `identity` directly in the temp DB so the
/// decision table sees a foreign or parallel session.
fn seed_active_lease(identity: &str, issue: u64, session: &str, worktree_path: &str) {
    let storage = Storage::open().expect("storage open");
    crate::worktree::ensure_schema(&storage).expect("schema");
    let now = now_unix_secs();
    insert_lease(
        &storage,
        NewLease {
            lease_id: "lease-seed",
            identity,
            issue,
            session,
            checkout_path: worktree_path,
            worktree_path,
            branch: "main",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("seed lease");
}

fn reuse_only_options<'a>(cache_base: &'a Path) -> AcquireOptions<'a> {
    AcquireOptions {
        cache_base: Some(cache_base),
        isolate: false,
        auto: false,
        base: None,
        reuse_only: true,
    }
}

#[test]
fn reuse_only_reuses_the_current_checkout_and_records_the_lease() {
    // Safe reuse and automatic lease bookkeeping continue: only directory
    // creation is opt-in (issue 616).
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-reuse") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-reuse");
    let cache = unique_cache("opt-in-reuse");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease_with(
        &runner,
        repo.dir.path(),
        616,
        "session-A",
        reuse_only_options(cache.path()),
    )
    .expect("a safe checkout must still be reused");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    assert!(
        outcome.warnings.is_empty(),
        "safe reuse must stay silent: {:?}",
        outcome.warnings
    );
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let rows = leases_for_repo(&identity).expect("list leases");
    assert_eq!(
        rows.len(),
        1,
        "the reuse must still record one active lease"
    );
    assert_eq!(rows[0].issue, 616);
    assert_eq!(rows[0].session, "session-A");
    assert!(
        !cache.path().join("worktrees").exists(),
        "reuse must not create a worktree directory"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_only_refuses_a_dirty_foreign_bound_checkout_with_guidance() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-dirty-foreign") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-dirty-foreign");
    let cache = unique_cache("opt-in-dirty-foreign");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let error = acquire_lease_with(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        reuse_only_options(cache.path()),
    )
    .expect_err("a dirty foreign checkout must not be created into implicitly");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("dirty")
            && error.message.contains("241")
            && error.message.contains("--isolate")
            && error.message.contains("issue 245"),
        "guidance must name the trigger and the explicit command: {error}"
    );
    assert!(
        error.message.chars().count() <= 200,
        "the bounded message must keep the whole command: {error}"
    );
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let rows = leases_for_repo(&identity).expect("list leases");
    assert!(
        rows.is_empty(),
        "a refused conflict must not book the checkout: {rows:?}"
    );
    assert!(
        !cache.path().join("worktrees").exists(),
        "a refused conflict must not create a worktree directory"
    );
    assert!(
        repo.dir.path().exists(),
        "a refused conflict must never delete the current checkout"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_only_refuses_a_parallel_session_on_a_dirty_checkout() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-parallel") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-parallel");
    let cache = unique_cache("opt-in-parallel");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 245);
    seed_active_lease(&identity, 245, "session-A", "/tmp/phasegent-parallel");
    let error = acquire_lease_with(
        &runner,
        repo.dir.path(),
        245,
        "session-B",
        reuse_only_options(cache.path()),
    )
    .expect_err("a parallel session's lease must refuse implicit creation");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("--isolate") && error.message.contains("issue 245"),
        "guidance must name the explicit command: {error}"
    );
    let rows = leases_for_repo(&identity).expect("list leases");
    assert_eq!(rows.len(), 1, "only the seeded lease may exist: {rows:?}");
    assert!(
        !cache.path().join("worktrees").exists(),
        "a refused conflict must not create a worktree directory"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_only_refuses_while_another_active_lease_exists() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-repo-lease") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-repo-lease");
    let cache = unique_cache("opt-in-repo-lease");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    seed_active_lease(&identity, 999, "other-session", "/tmp/phasegent-foreign");
    let error = acquire_lease_with(
        &runner,
        repo.dir.path(),
        616,
        "session-A",
        reuse_only_options(cache.path()),
    )
    .expect_err("another active lease must refuse implicit creation");
    assert_eq!(error.kind, "isolation");
    assert!(
        error
            .message
            .contains("another active lease exists for this repository")
            && error.message.contains("--isolate"),
        "unexpected guidance: {error}"
    );
    let rows = leases_for_repo(&identity).expect("list leases");
    assert_eq!(rows.len(), 1, "only the seeded lease may exist: {rows:?}");
    assert_eq!(rows[0].issue, 999);
    assert!(
        !cache.path().join("worktrees").exists(),
        "a refused conflict must not create a worktree directory"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_only_still_creates_when_explicit_isolation_opts_in() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-isolate") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-isolate");
    let cache = unique_cache("opt-in-isolate");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let outcome = acquire_lease_with(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: true,
            auto: false,
            base: None,
            reuse_only: true,
        },
    )
    .expect("explicit --isolate must win over the reuse-only gate");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .path
            .starts_with(cache.path().to_string_lossy().as_ref()),
        "the opted-in worktree must live under the cache base"
    );
    assert!(Path::new(&outcome.path).exists());
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_only_still_creates_when_worktree_auto_is_enabled() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-auto") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-auto");
    let cache = unique_cache("opt-in-auto");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let outcome = acquire_lease_with(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: false,
            auto: true,
            base: None,
            reuse_only: true,
        },
    )
    .expect("the resolved worktree-auto switch must win over the reuse-only gate");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
    assert!(Path::new(&outcome.path).exists());
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_only_keeps_the_idempotent_home_coming() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-idempotent") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-idempotent");
    let cache = unique_cache("opt-in-idempotent");
    let runner = ProcessWorktreeRunner::new();
    let first = acquire_lease_with(
        &runner,
        repo.dir.path(),
        616,
        "session-A",
        reuse_only_options(cache.path()),
    )
    .expect("first reuse");
    let second = acquire_lease_with(
        &runner,
        repo.dir.path(),
        616,
        "session-A",
        reuse_only_options(cache.path()),
    )
    .expect("second acquire must hit the idempotent home-coming");
    assert_eq!(second.reason, "idempotent");
    assert_eq!(first.lease_id, second.lease_id);
    assert_eq!(first.path, second.path);
    assert!(
        second.warnings.is_empty(),
        "idempotent reuse must stay silent: {:?}",
        second.warnings
    );
    drop(cache);
    drop(db_temp);
}
