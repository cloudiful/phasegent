use super::cli_support::*;
use super::support::*;
use super::*;

#[test]
fn acquire_dirty_foreign_bound_reuses_by_default_with_warning() {
    // Issue 651 P2: a dirty checkout bound to another issue reuses by
    // default. Dirt and bindings are advisory only; only an active
    // lease on the checkout path forces isolation.
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
    .expect("dirty + foreign-bound must reuse by default, not isolate");
    assert!(
        !outcome.created,
        "the lease-first default must reuse a dirty foreign-bound checkout"
    );
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("dirty") && joined.contains("advisory only"),
        "the advisory dirty warning must survive: {joined}"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "reuse must not create a worktree directory"
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
fn cli_acquire_reuse_flag_reuses_free_checkout() {
    // Issue 651 P3: `--reuse` states the default explicitly. Through
    // the parse → execute round-trip a free (dirty, foreign-bound)
    // checkout reuses: exit 0, one lease on the primary path, no
    // worktree directory.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-cli-reuse") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p3-cli-reuse");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let invocation = crate::command::parse_with_role_env(
        &strings([
            "worktree",
            "acquire",
            "--issue",
            "245",
            "--reuse",
            "--session",
            "session-A",
        ]),
        Some("orchestrator"),
    )
    .expect("parse --reuse");
    let Command::Worktree(command) = invocation.command else {
        panic!("--reuse must parse to an acquire command");
    };
    let exit = in_temp_repo(&repo, || {
        execute_worktree(Some(Role::Orchestrator), command)
    });
    assert_eq!(exit, 0, "--reuse on a free checkout must succeed");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(rows.len(), 1, "--reuse must record one lease");
    assert_eq!(
        rows[0].worktree_path,
        repo.dir.path().to_string_lossy().to_string(),
        "--reuse must book the primary checkout"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "--reuse must not create a worktree directory"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_acquire_reuse_flag_never_bypasses_occupied_path() {
    // The `--reuse` preference never overrides the safety boundary: a
    // second session stating `--reuse` on an occupied checkout still
    // isolates with exit 0 instead of sharing the path.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-cli-reuse-occupied") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) =
        open_temp_db_and_cache("p3-cli-reuse-occupied");
    let runner = ProcessWorktreeRunner::new();
    let first = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        false,
        false,
    )
    .expect("first acquire occupies the checkout");
    assert_eq!(first.reason, "no_conflict");
    let invocation = crate::command::parse_with_role_env(
        &strings([
            "worktree",
            "acquire",
            "--issue",
            "245",
            "--reuse",
            "--session",
            "session-B",
        ]),
        Some("orchestrator"),
    )
    .expect("parse --reuse");
    let Command::Worktree(command) = invocation.command else {
        panic!("--reuse must parse to an acquire command");
    };
    let exit = in_temp_repo(&repo, || {
        execute_worktree(Some(Role::Orchestrator), command)
    });
    assert_eq!(exit, 0, "--reuse on an occupied checkout must succeed");
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    let actives: Vec<_> = rows
        .iter()
        .filter(|row| row.status == LEASE_STATUS_ACTIVE)
        .collect();
    assert_eq!(actives.len(), 2, "both sessions hold live leases");
    assert_ne!(
        actives[0].worktree_path, actives[1].worktree_path,
        "--reuse must isolate onto a second path, never share the occupied one"
    );
    assert!(
        actives.iter().any(|row| row
            .worktree_path
            .starts_with(cache_temp.path().to_string_lossy().as_ref())),
        "the isolated worktree must land under the temp cache"
    );
}

#[test]
fn cli_acquire_reuse_and_isolate_together_are_rejected() {
    // Stating both preferences at once is contradictory: the parser
    // rejects the combination before any storage or git work.
    let _lock = lock_workflow_tests();
    let err = crate::command::parse_with_role_env(
        &strings([
            "worktree",
            "acquire",
            "--issue",
            "245",
            "--reuse",
            "--isolate",
        ]),
        Some("orchestrator"),
    )
    .expect_err("--reuse --isolate must not parse");
    assert!(
        err.contains("--reuse") && err.contains("--isolate"),
        "the rejection must name both spellings: {err}"
    );
}

#[test]
fn leftover_worktree_auto_row_and_env_are_ignored_by_acquire() {
    // Issue 651 P3 removal proof: a `PHASEGENT_WORKTREE_AUTO` row left
    // behind by an older version (plus the environment value) is inert
    // data. The row is preserved — never deleted — but acquire consults
    // only the lease-first table, so a dirty foreign-bound checkout on
    // a free path reuses where the old switch would have isolated.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("p3-auto-leftover") else {
        return;
    };
    let (_db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("p3-auto-leftover");
    let storage = Storage::open().expect("storage open");
    storage
        .save_global_setting("PHASEGENT_WORKTREE_AUTO", "true")
        .expect("leftover row must persist");
    let _auto_env = EnvGuard::set("PHASEGENT_WORKTREE_AUTO", "true");
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
    .expect("a free checkout must reuse despite the leftover setting");
    assert!(
        !outcome.created,
        "the removed switch must not isolate: {outcome:?}"
    );
    assert_eq!(outcome.reason, "no_conflict");
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "reuse must not create a worktree directory"
    );
    let stored = storage
        .load_global_setting("PHASEGENT_WORKTREE_AUTO")
        .expect("leftover row must still read");
    assert_eq!(
        stored.as_deref(),
        Some("true"),
        "removal never deletes the persisted row"
    );
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn worktree_auto_setting_is_rejected_as_unknown() {
    // The config surface is gone with the switch: neither the
    // `PHASEGENT_*` name nor the kebab-case alias canonicalizes, so
    // `config set`/`clear` reject the key before any role or storage
    // work instead of persisting it.
    let _lock = lock_workflow_tests();
    for spelling in ["PHASEGENT_WORKTREE_AUTO", "worktree-auto"] {
        assert_eq!(
            crate::config_write::canonical_setting_name(spelling),
            None,
            "{spelling} must not canonicalize after removal"
        );
    }
}

// ---------------------------------------------------------------------------
// Issue 651 P3: coherent reuse/isolate CLI-surface contract tests
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
// The states covered:
//   1. Default: dirty + foreign-bound + no flags -> exit 0, lease row
//      inserted on the current checkout, no worktree dir under the
//      temp cache.
//   2. `--reuse`: the same free checkout reuses through the parse →
//      execute round-trip; on an occupied checkout `--reuse` still
//      isolates instead of bypassing the active lease.
//   3. `--isolate`: dirty + foreign-bound + `--isolate` -> exit 0,
//      lease row inserted, worktree dir under the temp cache.
//   4. `--reuse --isolate` together is a parser rejection, and the
//      removed `PHASEGENT_WORKTREE_AUTO` setting (row or env) is
//      ignored by acquire and rejected by `config set`/`clear`.
