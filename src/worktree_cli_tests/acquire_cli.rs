use super::support::*;
use super::*;

#[test]
fn cli_acquire_dirty_foreign_bound_returns_new_worktree_envelope_with_warning() {
    // Issue #246 replica through the CLI-facing contract: empty lease
    // table + dirty checkout bound to 241 + acquiring 245 -> the CLI
    // envelope must say created:true / reason:"new_worktree" and the
    // stderr-bound warning must carry the bound-issue detail.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-dirty-foreign") else {
        return;
    };
    let (_db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-dirty-foreign");
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
    .expect("dirty + foreign-bound must isolate, not error");
    assert!(
        outcome.created,
        "dirty + bound-to-241 must create a worktree for 245"
    );
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "new worktree must live under the temp cache"
    );
    assert!(outcome.branch.starts_with("phasegent/245-"));
    let warning_text = outcome.warnings.join(" ");
    assert!(
        outcome.warnings.iter().any(|w| w.contains("241")),
        "trigger detail must reach the stderr warning payload: {warning_text}"
    );
    assert_cli_envelope(outcome, true, "new_worktree");
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_acquire_dirty_unbound_returns_reuse_envelope_with_warning() {
    // Dirty + unbound: no task evidence -> the CLI envelope must say
    // created:false / reason:"no_conflict" (reuse) and the stderr-bound
    // warning must explain the reused tree is not pristine.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-dirty-unbound") else {
        return;
    };
    let (_db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-dirty-unbound");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
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
    .expect("dirty + unbound must reuse, not error");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("dirty") && joined.contains("not bound"),
        "reuse of a dirty unbound checkout must carry a warning: {joined}"
    );
    assert_cli_envelope(outcome, false, "no_conflict");
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_acquire_envelope_is_unchanged_when_warnings_are_present() {
    // Regression guard for the "CLI envelope fields unchanged" rule: even
    // when the outcome carries warnings (dirty-tree triggers), the JSON the
    // CLI prints must contain exactly lease_id/path/branch/repo_identity/
    // created/reason and never a warnings key.
    let outcome = AcquireOutcome {
        lease_id: "lease-x".to_owned(),
        path: "/tmp/cache/wt".to_owned(),
        branch: "phasegent/245-abcdef".to_owned(),
        repo_identity: "/tmp/r/.git".to_owned(),
        created: true,
        reason: "new_worktree".to_owned(),
        warnings: vec![
            "checkout is dirty and bound to issue 241; acquiring issue 245 \
             in an isolated worktree"
                .to_owned(),
            "checkout is dirty and not bound to an issue; reusing it \
             (no task evidence of a conflict)"
                .to_owned(),
        ],
    };
    let json = serde_json::to_value(AcquireJson::from(outcome)).expect("envelope serialise");
    let text = json.to_string();
    assert!(
        !text.contains("warnings"),
        "CLI envelope must not leak warnings: {text}"
    );
    assert_eq!(json["lease_id"], serde_json::json!("lease-x"));
    assert_eq!(json["path"], serde_json::json!("/tmp/cache/wt"));
    assert_eq!(json["branch"], serde_json::json!("phasegent/245-abcdef"));
    assert_eq!(json["repo_identity"], serde_json::json!("/tmp/r/.git"));
    assert_eq!(json["created"], serde_json::json!(true));
    assert_eq!(json["reason"], serde_json::json!("new_worktree"));
}

#[test]
fn cli_acquire_executor_resolves_cache_through_env_override() {
    // The CLI executor passes `cache_base = None`; production resolution
    // must route through `PHASEGENT_WORKTREE_CACHE_DIR` when set. This
    // proves the executor's env-driven cache path lands in the temp dir
    // rather than the real OS cache (temp-only guarantee).
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-cache-env") else {
        return;
    };
    let (db_temp, cache_temp, db_env, cache_env) = open_temp_db_and_cache("cli-cache-env");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 7);
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
    .expect("acquire through env-resolved cache");
    assert!(outcome.created);
    assert!(
        outcome
            .path
            .starts_with(cache_temp.path().to_string_lossy().as_ref()),
        "executor cache resolution must honour PHASEGENT_WORKTREE_CACHE_DIR"
    );
    drop((db_temp, cache_temp, db_env, cache_env));
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn cli_acquire_dirty_foreign_bound_recorded_in_temp_db_only() {
    // Temp-only guarantee: after a dirty + foreign-bound acquire through
    // the CLI-facing path, the lease row must live in the temp database
    // (queryable under the resolved repo identity) and never touch the
    // operator's real database.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-temp-db") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-temp-db");
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        None,
        true,
        false,
    )
    .expect("dirty + foreign-bound acquire");
    let storage = Storage::open_at(&db_temp.path().join("phasegent.sqlite3")).expect("storage");
    ensure_schema(&storage).expect("ensure_schema");
    let rows = list_for_repo(&storage, &identity).expect("list temp db");
    assert_eq!(
        rows.len(),
        1,
        "one active lease must be recorded in the temp DB"
    );
    assert_eq!(rows[0].issue, 245);
    assert_eq!(rows[0].lease_id, outcome.lease_id);
    assert_eq!(rows[0].worktree_path, outcome.path);
    assert_eq!(rows[0].status, LEASE_STATUS_ACTIVE);
    let _ = std::fs::remove_file(&scratch);
}

// ---------------------------------------------------------------------------
// Issue #247: worktree-auto switch + `--isolate` gating, updated by
// issue #436
// ---------------------------------------------------------------------------
//
// Issue #436 flips the acquire default: a dirty checkout (or any other
// active lease) isolates even when `--isolate`/`worktree-auto` are off,
// while `--isolate` and the resolved switch stay accepted and keep
// working. The switch itself still decides the `Unknown` `git status`
// probe state, and its env-over-SQLite resolution is unchanged. All
// tests pin their DB and cache to temp dirs.
