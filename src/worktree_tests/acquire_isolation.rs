use super::support::*;
use super::*;

#[test]
fn acquire_second_session_isolates_instead_of_colliding_on_reuse() {
    // Issue 437 symptom: with the old default, two acquires for different
    // sessions both reused the same checkout, so the second lease insert
    // hit the `(repo_identity, worktree_path)` unique index. Issue #436
    // flips the default: the second session now gets its own isolated
    // worktree, so the collision (and any raw SQLite text) never happens.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("reuse-conflict") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-reuse-conflict");
    let cache = unique_cache("reuse-conflict");
    let runner = ProcessWorktreeRunner::new();
    let first = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("first acquire reuses the current checkout");
    assert_eq!(first.reason, "no_conflict");
    let second = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-B",
        Some(cache.path()),
        false,
        false,
    )
    .expect("a second session must isolate, not collide on the checkout lease");
    assert!(
        second.created,
        "the second session must get a fresh worktree"
    );
    assert_eq!(second.reason, "new_worktree");
    assert_ne!(second.path, first.path);
    assert!(second.branch.starts_with("phasegent/239-"));
    assert!(
        second
            .warnings
            .iter()
            .all(|warning| !warning.contains("UNIQUE constraint failed")),
        "raw SQLite text must never reach the operator: {:?}",
        second.warnings
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_new_worktree_conflict_reports_guidance_and_compensates() {
    // When the freshly added worktree's lease insert fails, the
    // compensation block removes the orphan directory and must return
    // the same guidance. The generated branch slug is random, so a
    // BEFORE INSERT trigger reproduces the unique-index failure
    // deterministically in the temp database.
    let _lock = lock_workflow_tests();
    let (db_temp, storage, _env) = open_temp_db("acquire-new-wt-conflict");
    crate::worktree::ensure_schema(&storage).expect("ensure_schema");
    let identity = "/tmp/phasegent-437-new-wt-repo/.git";
    let now = now_unix_secs();
    insert_lease(
        &storage,
        NewLease {
            lease_id: "lease-seed",
            identity,
            issue: 7,
            session: "seed",
            checkout_path: "/tmp/phasegent-437-new-wt-repo",
            worktree_path: "/tmp/phasegent-437-other",
            branch: "phasegent/7-seed",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("seed lease");
    storage
        .connection
        .execute_batch(
            "CREATE TRIGGER force_repo_path_conflict BEFORE INSERT ON worktree_leases \
             WHEN NEW.lease_id != 'lease-seed' \
             BEGIN SELECT RAISE(ABORT, 'UNIQUE constraint failed: \
             worktree_leases.worktree_leases_repo_path_idx'); END;",
        )
        .expect("conflict trigger");
    let repo_path = PathBuf::from("/tmp/phasegent-437-new-wt-repo");
    let cache = unique_cache("new-wt-conflict");
    let runner = FakeWorktreeRunner::new(vec![
        FakeResponse {
            args: vec!["rev-parse".to_string(), "--git-common-dir".to_string()],
            status: 0,
            stdout: ".git".to_string(),
        },
        FakeResponse {
            args: vec!["status".to_string(), "--porcelain".to_string()],
            status: 0,
            stdout: String::new(),
        },
        FakeResponse {
            args: vec!["worktree".to_string(), "add".to_string()],
            status: 0,
            stdout: String::new(),
        },
        FakeResponse {
            args: vec!["worktree".to_string(), "remove".to_string()],
            status: 0,
            stdout: String::new(),
        },
    ]);
    let error = acquire_lease(
        &runner,
        &repo_path,
        245,
        "session-A",
        Some(cache.path()),
        true,
        false,
    )
    .expect_err("the forced insert conflict must surface as an error");
    assert_eq!(error.kind, "storage");
    assert!(
        error.message.contains("--isolate")
            && error.message.contains("worktree status")
            && error.message.contains("worktree list"),
        "guidance must reach the new-worktree path: {error}"
    );
    let calls = runner.recorded();
    assert!(
        calls.iter().any(|(args, _)| {
            args.first().map(String::as_str) == Some("worktree")
                && args.get(1).map(String::as_str) == Some("remove")
        }),
        "the orphan worktree must be compensated: {calls:?}"
    );
    drop(cache);
    drop(db_temp);
}

// ---------------------------------------------------------------------------
// Cache root test (filesystem-only).
// ---------------------------------------------------------------------------

#[test]
fn acquire_dirty_foreign_bound_creates_isolated_worktree_on_empty_table() {
    // Issue #246 replica: empty lease table + dirty checkout bound to
    // issue 241, acquiring issue 245 must NOT reuse the dirty tree.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("dirty-foreign-bound") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-dirty-foreign-bound");
    let cache = unique_cache("dirty-foreign-bound");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        Some(cache.path()),
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
            .starts_with(cache.path().to_string_lossy().as_ref()),
        "new worktree must live under the cache base"
    );
    assert!(outcome.branch.starts_with("phasegent/245-"));
    assert!(Path::new(&outcome.path).exists(), "worktree dir must exist");
    assert!(
        outcome.warnings.iter().any(|w| w.contains("241")),
        "trigger detail (dirty + bound #N) belongs in warnings: {:?}",
        outcome.warnings
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_dirty_foreign_bound_isolates_by_default_without_isolate_flag() {
    // Issue #436 behavior change: a dirty checkout bound to another issue
    // isolates even when neither `--isolate` nor `worktree-auto` is set.
    // The warning must name the new default and the retained flag.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("dirty-default-isolate") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-dirty-default-isolate");
    let cache = unique_cache("dirty-default-isolate");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("dirty + foreign-bound must isolate by default, not reuse");
    assert!(
        outcome.created,
        "a dirty foreign-bound checkout must not be reused by default"
    );
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .path
            .starts_with(cache.path().to_string_lossy().as_ref()),
        "the isolated worktree must live under the cache base"
    );
    assert!(Path::new(&outcome.path).exists(), "worktree dir must exist");
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("241")
            && joined.contains("auto-isolation now defaults on")
            && joined.contains("--isolate"),
        "the warning must document the new default and the retained flag: {joined}"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_auto_binds_new_worktree_branch_and_installs_hooks() {
    // Issue #436 single-command closure: after an isolated acquire the
    // fresh branch is bound to the requested issue and the managed commit
    // hooks are installed, while the main checkout's branch stays
    // untouched. The origin gate of `lifecycle::auto_install_hooks` is
    // satisfied by giving the temp repo an origin.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("auto-bind-hooks") else {
        return;
    };
    repo.set_origin("https://git.example/acme/widgets.git");
    let (db_temp, _storage, _env) = open_temp_db("acquire-auto-bind-hooks");
    let cache = unique_cache("auto-bind-hooks");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("dirty checkout must acquire an isolated worktree");
    assert!(outcome.created);
    assert!(outcome.branch.starts_with("phasegent/245-"));
    let checkout = Path::new(&outcome.path);
    let git_runner = crate::branch_context::ProcessGitRunner::in_directory(checkout);
    let bound = crate::branch_context::read_issue_id(&git_runner, &outcome.branch)
        .expect("binding read in the new worktree");
    assert_eq!(
        bound,
        Some(245),
        "the fresh branch must be auto-bound to the acquired issue"
    );
    let main_runner = crate::branch_context::ProcessGitRunner::in_directory(repo.dir.path());
    let main_bound = crate::branch_context::read_issue_id(&main_runner, &repo.head_branch)
        .expect("binding read in the main checkout");
    assert_eq!(
        main_bound,
        Some(241),
        "auto-bind must target the new checkout, not the origin checkout"
    );
    assert!(
        outcome
            .warnings
            .iter()
            .all(|warning| !warning.contains("not bound") && !warning.contains("hook")),
        "a clean bind + hook install must stay quiet: {:?}",
        outcome.warnings
    );
    #[cfg(unix)]
    for name in ["prepare-commit-msg", "commit-msg"] {
        assert!(
            hook_path_in(checkout, name).is_file(),
            "{name} must be installed in the acquired checkout"
        );
    }
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}
