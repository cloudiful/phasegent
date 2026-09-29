use super::cli_support::*;
use super::support::*;
use super::*;

#[test]
fn cli_surface_acquire_dirty_foreign_bound_reuses_by_default() {
    // Issue 651 P2 through the CLI surface: dirty + bound-to-241 +
    // acquiring 245 with no `--isolate` reuses the primary checkout.
    // The lease row names the current branch (not a generated one)
    // and no worktree directory is created.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-default-isolate") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) =
        open_temp_db_and_cache("p2-cli-default-isolate");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit =
        run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), false);
    assert_eq!(
        exit, 0,
        "the lease-first default acquire through the CLI surface must succeed"
    );
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(
        rows.len(),
        1,
        "the default CLI acquire must record one lease"
    );
    assert_eq!(rows[0].issue, 245);
    assert_eq!(
        rows[0].worktree_path,
        repo.dir.path().to_string_lossy().to_string(),
        "a dirty foreign-bound checkout must reuse the primary without --isolate"
    );
    assert_eq!(
        rows[0].branch, repo.head_branch,
        "reuse keeps the current branch instead of a generated one"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "the default CLI acquire must not create a worktree directory"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_surface_acquire_isolate_flag_creates_new_worktree_in_temp_cache() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-isolate") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p2-cli-isolate");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit = run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), true);
    assert_eq!(exit, 0, "--isolate CLI acquire must succeed");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1, "--isolate CLI acquire must record one lease");
    assert_eq!(rows[0].issue, 245);
    assert!(
        rows[0].branch.starts_with("phasegent/245-"),
        "--isolate CLI acquire must use the new-issue branch slug"
    );
    assert!(
        rows[0]
            .worktree_path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "--isolate CLI acquire must land under the temp cache, not the operator's real cache"
    );
    assert!(
        std::path::Path::new(&rows[0].worktree_path).exists(),
        "--isolate CLI acquire must create the worktree directory on disk"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_surface_acquire_ignores_worktree_auto_env_and_reuses() {
    // Issue 651 P5: the removed `PHASEGENT_WORKTREE_AUTO` setting is
    // inert at the CLI surface too. With it set, a dirty foreign-bound
    // checkout on a free path still reuses: exit 0, one lease on the
    // primary checkout under the current branch, no worktree directory.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-env-auto") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p2-cli-env-auto");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "true");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit =
        run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), false);
    assert_eq!(exit, 0, "inert-env CLI acquire must succeed");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1, "inert-env CLI acquire must record one lease");
    assert_eq!(rows[0].issue, 245);
    assert_eq!(
        rows[0].worktree_path,
        repo.dir.path().to_string_lossy().to_string(),
        "a removed switch must not isolate a free checkout"
    );
    assert_eq!(
        rows[0].branch, repo.head_branch,
        "reuse keeps the current branch instead of a generated one"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "inert-env CLI acquire must not create a worktree directory"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_acquire_explicit_session_overrides_environment_and_persists() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-session-explicit") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) =
        open_temp_db_and_cache("p2-cli-session-explicit");
    let _session_env = EnvGuard::set("PHASEGENT_SESSION_ID", "env-session");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit = run_cli_acquire_with_session(&repo, false, Some("explicit-session"));
    assert_eq!(exit, 0, "explicit-session acquire must succeed");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].session, "explicit-session",
        "--session must win over PHASEGENT_SESSION_ID"
    );
}

#[test]
fn cli_acquire_uses_environment_session_when_flag_absent() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-session-env") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p2-cli-session-env");
    let _session_env = EnvGuard::set("PHASEGENT_SESSION_ID", "env-session");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit = run_cli_acquire_with_session(&repo, false, None);
    assert_eq!(exit, 0, "environment-session acquire must succeed");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].session, "env-session",
        "PHASEGENT_SESSION_ID must supply the session when --session is absent"
    );
}

#[test]
fn cli_acquire_without_identity_fails_clearly() {
    // Issue 651 P4: with no `--session` and no usable
    // `PHASEGENT_SESSION_ID`, acquire fails fast with exit 2 instead of
    // booking the checkout under a fabricated shared owner. A blank
    // environment value counts as missing, which keeps the test
    // deterministic however the host environment looks. Nothing is
    // recorded and no worktree is created.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p4-cli-no-identity") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p4-cli-no-identity");
    let _session_env = EnvGuard::set("PHASEGENT_SESSION_ID", "");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit = run_cli_acquire_with_session(&repo, false, None);
    assert_eq!(
        exit, 2,
        "a missing session identity must fail with a usage error"
    );
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert!(
        rows.is_empty(),
        "a refused acquire must not book the checkout: {rows:?}"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "a refused acquire must not create a worktree directory"
    );
}

#[test]
fn cli_acquire_second_session_isolates_and_avoids_the_duplicate_lease() {
    // Issue 437 surfaced the raw `(repo_identity, worktree_path)` unique
    // violation when two sessions reused one checkout. The lease-first
    // table (issue 651 P2) removes the scenario: the second session sees
    // the active lease on the checkout path and isolates with exit 0, so
    // the CLI never reports a storage error for a legitimate parallel
    // acquire. The guidance translation itself stays covered by the
    // storage-level regression tests.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p2-cli-dup-reuse") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p2-cli-dup-reuse");
    let first = run_cli_acquire_with_session(&repo, false, Some("session-A"));
    assert_eq!(first, 0, "the first reuse-current acquire must succeed");
    let second = run_cli_acquire_with_session(&repo, false, Some("session-B"));
    assert_eq!(
        second, 0,
        "the second session must isolate by default instead of colliding"
    );
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(
        rows.len(),
        2,
        "both sessions must hold their own lease row: {rows:?}"
    );
    assert_ne!(
        rows[0].worktree_path, rows[1].worktree_path,
        "the two leases must not share one checkout path"
    );
}

// ---------------------------------------------------------------------------
// Issue 305 Task 4: unknown `git status` through the CLI surface
// ---------------------------------------------------------------------------
//
// A corrupt `.git/index` makes `git status --porcelain` fail while
// `git rev-parse --git-common-dir` and `git worktree add` keep working,
// so the real `execute_worktree` path can be driven into the `Unknown`
// dirty state. Explicit `--isolate` must create an isolated worktree;
// the default must reuse the current checkout, never silently
// treating the checkout as clean.

#[test]
fn cli_surface_acquire_unknown_status_with_isolate_creates_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("t4-cli-unknown-auto") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("t4-cli-unknown-auto");
    corrupt_git_index(&repo);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit = run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), true);
    assert_eq!(
        exit, 0,
        "unknown status + --isolate CLI acquire must succeed"
    );
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1, "CLI acquire must record one lease");
    assert_eq!(rows[0].issue, 245);
    assert!(
        rows[0].branch.starts_with("phasegent/245-"),
        "unknown status with isolation must create a fresh worktree"
    );
    assert!(
        rows[0]
            .worktree_path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "isolated worktree must land under the temp cache"
    );
    assert!(
        std::path::Path::new(&rows[0].worktree_path).exists(),
        "unknown status with isolation must create the worktree directory"
    );
}

#[test]
fn cli_surface_acquire_unknown_status_default_off_reuses_checkout() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("t4-cli-unknown-off") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("t4-cli-unknown-off");
    corrupt_git_index(&repo);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let exit =
        run_cli_acquire_in_temp_repo(&repo, &db_temp.path().join("phasegent.sqlite3"), false);
    assert_eq!(
        exit, 0,
        "unknown status + default off CLI acquire must succeed"
    );
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1, "CLI acquire must record one lease");
    assert_eq!(rows[0].issue, 245);
    assert_eq!(
        rows[0].worktree_path,
        repo.dir.path().to_string_lossy().to_string(),
        "default-off unknown status must reuse the current checkout"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "default-off unknown status must not create a worktree directory"
    );
}

// ---------------------------------------------------------------------------
// Issue 305 Task 2 / issue 337 Phase 1: heartbeat + prune CLI surface
// ---------------------------------------------------------------------------
