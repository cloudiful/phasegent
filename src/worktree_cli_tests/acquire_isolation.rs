use super::support::*;
use super::*;

#[test]
fn acquire_dirty_foreign_bound_isolates_by_default_with_warning() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-default-isolate") else {
        return;
    };
    let (_db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-default-isolate");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        false,
    )
    .expect("dirty + foreign-bound must isolate by default, not reuse");
    assert!(
        outcome.created,
        "the issue #436 default must create a worktree without --isolate"
    );
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "default isolation must land under the temp cache"
    );
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("241") && joined.contains("auto-isolation now defaults on"),
        "conflict warning must name the trigger and the new default: {joined}"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn acquire_isolate_flag_creates_new_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-isolate") else {
        return;
    };
    let (_db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-isolate");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        true,
        false,
    )
    .expect("--isolate must create, not error");
    assert!(outcome.created, "--isolate must create a worktree");
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "new worktree must live under the temp cache"
    );
    assert!(outcome.branch.starts_with("phasegent/245-"));
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn acquire_env_worktree_auto_true_creates_new_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-env-auto") else {
        return;
    };
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-env-auto");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "true");
    let storage = Storage::open().expect("storage open");
    let auto = resolve_worktree_auto(&storage).expect("resolve worktree-auto");
    assert!(auto, "PHASEGENT_WORKTREE_AUTO=true must resolve true");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        auto,
    )
    .expect("auto=true must create, not error");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn acquire_sqlite_worktree_auto_true_creates_new_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-sqlite-auto") else {
        return;
    };
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-sqlite-auto");
    let storage = Storage::open().expect("storage open");
    crate::config_write::set_setting_value(None, "PHASEGENT_WORKTREE_AUTO", "true", &storage)
        .expect("config set worktree-auto true");
    let auto = resolve_worktree_auto(&storage).expect("resolve worktree-auto");
    assert!(auto, "SQLite worktree-auto=true must resolve true");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        auto,
    )
    .expect("SQLite auto=true must create, not error");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn resolve_worktree_auto_defaults_false_and_env_false_overrides_sqlite_true() {
    let _lock = lock_workflow_tests();
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-precedence");
    let storage = Storage::open().expect("storage open");
    // Empty env behaves as unset -> default false.
    {
        let _unset = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "");
        assert!(
            !resolve_worktree_auto(&storage).expect("resolve default"),
            "unset worktree-auto must default false"
        );
    }
    crate::config_write::set_setting_value(None, "PHASEGENT_WORKTREE_AUTO", "true", &storage)
        .expect("config set worktree-auto true");
    assert!(
        resolve_worktree_auto(&storage).expect("resolve sqlite true"),
        "SQLite worktree-auto=true must resolve true"
    );
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "false");
    assert!(
        !resolve_worktree_auto(&storage).expect("resolve env false"),
        "env false must override SQLite true"
    );
}

#[test]
fn acquire_env_false_over_sqlite_true_still_isolates_dirty_conflict() {
    // The env-over-SQLite precedence is asserted on the resolver itself
    // (`resolve_worktree_auto_defaults_false_and_env_false_overrides_sqlite_true`).
    // Since issue #436 a resolved `false` no longer restores reuse for a
    // confirmed conflict trigger, so the dirty checkout still isolates.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p1-precedence-reuse") else {
        return;
    };
    let (_db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p1-precedence-reuse");
    let storage = Storage::open().expect("storage open");
    crate::config_write::set_setting_value(None, "PHASEGENT_WORKTREE_AUTO", "true", &storage)
        .expect("config set worktree-auto true");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "false");
    let auto = resolve_worktree_auto(&storage).expect("resolve worktree-auto");
    assert!(!auto, "env false must override SQLite true");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        auto,
    )
    .expect("a dirty conflict must isolate regardless of the resolved switch");
    assert!(
        outcome.created,
        "the conflict trigger must isolate even when worktree-auto resolves false"
    );
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "the isolated worktree must land under the temp cache"
    );
    let _ = std::fs::remove_file(&scratch);
}

// ---------------------------------------------------------------------------
// Issue #247 Phase 2: three-state CLI-surface contract tests
// ---------------------------------------------------------------------------
//
// These tests drive the CLI executor (`execute_worktree`) end-to-end so the
// contract the help text advertises is exercised through the same code path
// the operator's shell runs. Each test pins its DB and cache to temp dirs
// (temp-only guarantee), chdirs into a temp repo so the executor's
// `std::env::current_dir()` resolves to a sandboxed checkout, and asserts on
// the exit code together with the post-condition (lease row in the temp DB
// + presence/absence of the worktree dir under the temp cache).
//
// The three states covered:
//   1. Default (issue #436): dirty + foreign-bound + no `--isolate` + no
//      env + no SQLite setting -> exit 0, lease row inserted, worktree
//      dir under the temp cache.
//   2. `--isolate`: dirty + foreign-bound + `--isolate=true` -> exit 0,
//      lease row inserted, worktree dir under the temp cache.
//   3. Env true: dirty + foreign-bound + `PHASEGENT_WORKTREE_AUTO=true` ->
//      exit 0, lease row inserted, worktree dir under the temp cache.
