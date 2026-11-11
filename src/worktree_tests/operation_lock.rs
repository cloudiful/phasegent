use super::support::*;
use super::*;

fn cache_env(label: &str) -> (TempDir, EnvGuard) {
    let cache = unique_cache(label);
    let guard = EnvGuard::set(
        "PHASEGENT_WORKTREE_CACHE_DIR",
        cache.path().to_string_lossy().as_ref(),
    );
    (cache, guard)
}

#[test]
fn one_repository_allows_only_one_operation_at_a_time() {
    let _lock = lock_workflow_tests();
    let (cache, _env) = cache_env("op-lock-contention");
    let first = crate::worktree::operation_lock::acquire("/tmp/repo/.git").expect("first lock");
    let path = first.path().to_path_buf();
    let error = crate::worktree::operation_lock::acquire("/tmp/repo/.git")
        .err()
        .expect("a second operation must not acquire the same repository");
    assert_eq!(error.kind, "busy");
    assert!(
        error
            .message
            .contains("another phasegent worktree operation")
    );
    drop(first);
    crate::worktree::operation_lock::acquire("/tmp/repo/.git").expect("drop releases the lock");
    assert!(path.exists(), "the lock file is never unlinked");
    assert!(cache.path().join("worktrees").exists());
}

#[test]
fn different_repositories_do_not_block_each_other() {
    let _lock = lock_workflow_tests();
    let (_cache, _env) = cache_env("op-lock-independent");
    let _first = crate::worktree::operation_lock::acquire("/tmp/repo-a/.git").expect("first");
    let _second = crate::worktree::operation_lock::acquire("/tmp/repo-b/.git").expect("second");
}

#[test]
fn the_lock_file_path_is_stable_for_one_identity() {
    let _lock = lock_workflow_tests();
    let (_cache, _env) = cache_env("op-lock-stable");
    let lock = crate::worktree::operation_lock::acquire("/tmp/repo/.git").expect("lock");
    let expected = crate::worktree::compute_fingerprint("/tmp/repo/.git");
    assert!(
        lock.path()
            .file_name()
            .is_some_and(|name| name.to_string_lossy() == format!("{expected}.lock")),
        "the lock is keyed by the canonical repository fingerprint"
    );
}
