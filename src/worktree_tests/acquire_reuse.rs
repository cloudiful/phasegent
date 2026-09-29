use super::support::*;
use super::*;
use crate::worktree::{AcquireOptions, acquire_lease_with};

/// Seed one active lease row for `identity` on `worktree_path`
/// directly in the temp DB so the decision table sees an occupant.
fn seed_active_lease_on_path(identity: &str, issue: u64, session: &str, worktree_path: &str) {
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
            branch: "phasegent/1-seed",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("seed lease");
}

#[test]
fn acquire_reuses_current_checkout_when_no_other_lease_is_active() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("no-conflict") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-no-conflict");
    let cache = unique_cache("acquire-no-conflict");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("acquire no_conflict");
    let expected_path = repo.dir.path().to_string_lossy().to_string();
    assert_eq!(outcome.reason, "no_conflict");
    assert!(!outcome.created);
    assert_eq!(outcome.path, expected_path);
    assert_eq!(outcome.branch, repo.head_branch);
    assert!(!outcome.lease_id.is_empty());
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_is_idempotent_for_the_same_repo_issue_session() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("idempotent") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-idempotent");
    let cache = unique_cache("acquire-idempotent");
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
    .expect("first acquire");
    let second = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("second acquire");
    assert_eq!(
        first.lease_id, second.lease_id,
        "idempotent acquire must reuse the lease"
    );
    assert_eq!(first.path, second.path);
    assert_eq!(first.branch, second.branch);
    assert_eq!(second.reason, "idempotent");
    assert!(
        first.warnings.is_empty() && second.warnings.is_empty(),
        "idempotent reuse on a clean tree must not warn"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_creates_a_new_worktree_for_a_second_session() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("new-worktree") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-new");
    let cache = unique_cache("acquire-new");
    let runner = ProcessWorktreeRunner::new();
    // Issue #509: explicit `--isolate` now forces a fresh worktree even on
    // a clean empty table, so the first acquire uses the default path to
    // keep covering `no_conflict` reuse; the second/third still isolate.
    let first = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("first acquire");
    assert_eq!(first.reason, "no_conflict");
    let second = acquire_lease(
        &runner,
        repo.dir.path(),
        239,
        "session-B",
        Some(cache.path()),
        true,
        false,
    )
    .expect("second acquire");
    assert_eq!(second.reason, "new_worktree");
    assert!(second.created);
    assert_ne!(second.lease_id, first.lease_id);
    assert_ne!(second.path, first.path);
    assert!(
        second
            .path
            .starts_with(cache.path().to_string_lossy().as_ref())
    );
    assert!(second.branch.starts_with("phasegent/239-"));
    assert!(Path::new(&second.path).exists());
    let third = acquire_lease(
        &runner,
        repo.dir.path(),
        240,
        "session-A",
        Some(cache.path()),
        true,
        false,
    )
    .expect("third acquire");
    assert_eq!(third.reason, "new_worktree");
    assert!(third.branch.starts_with("phasegent/240-"));
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_clean_foreign_bound_reuses_current_checkout_without_overwriting_binding() {
    // A clean checkout bound to another issue is not contaminated, so it
    // is still reused (no_conflict). The post-acquire auto-bind must not
    // overwrite that foreign binding: `branch_context::bind` reports its
    // usual conflict, which acquire degrades to a warning.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("clean-foreign-bound") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-clean-foreign-bound");
    let cache = unique_cache("clean-foreign-bound");
    let runner = ProcessWorktreeRunner::new();
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
    .expect("acquire on a clean foreign-bound checkout");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("issue 241") && joined.contains("--replace"),
        "auto-bind must surface the foreign-binding conflict instead of overwriting: {joined}"
    );
    let git_runner = crate::branch_context::ProcessGitRunner::in_directory(repo.dir.path());
    let bound =
        crate::branch_context::read_issue_id(&git_runner, &repo.head_branch).expect("binding read");
    assert_eq!(
        bound,
        Some(241),
        "the foreign binding must survive auto-bind"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_dirty_unbound_reuses_current_checkout_with_warning() {
    // Dirty + unbound: dirt is advisory only, so reuse the checkout
    // but surface a best-effort warning that the tree is not pristine.
    // Branch ownership is never consulted.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("dirty-unbound") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-dirty-unbound");
    let cache = unique_cache("dirty-unbound");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("dirty + unbound must reuse, not error");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("dirty") && joined.contains("advisory only"),
        "reuse of a dirty checkout must carry the advisory warning: {joined}"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_dirty_same_issue_other_session_lease_creates_new_worktree() {
    // Two parallel sessions for the same issue must not share one
    // checkout path: once session A has recorded a lease on the
    // primary, session B sees the path-scoped conflict and gets its
    // own worktree (issue 651 P2).
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("dirty-same-issue") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-dirty-same-issue");
    let cache = unique_cache("dirty-same-issue");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 245);
    let first = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("first session acquire");
    // No lease exists yet: the checkout path is free, so the dirty
    // tree reuses even though it is bound to our own issue.
    assert!(!first.created);
    assert_eq!(first.reason, "no_conflict");
    let second = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-B",
        Some(cache.path()),
        false,
        false,
    )
    .expect("second session acquire");
    assert!(
        second.created,
        "a second live session on the same checkout path must isolate"
    );
    assert_eq!(second.reason, "new_worktree");
    assert!(
        second
            .path
            .starts_with(cache.path().to_string_lossy().as_ref())
    );
    assert!(second.branch.starts_with("phasegent/245-"));
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_dirty_bound_same_issue_keeps_stale_branch_binding() {
    // Issue 305 Task 4: a dirty checkout bound to this issue with no
    // active lease is a crashed predecessor's work. Acquire reuses it
    // (bindings never decide under issue 651 P2) and must keep the
    // stale branch binding as safe evidence instead of clearing it.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("stale-binding") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-stale-binding");
    let cache = unique_cache("stale-binding");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 245);
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("dirty + same-issue with no other lease must reuse");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    let git_runner = crate::branch_context::ProcessGitRunner::in_directory(repo.dir.path());
    let bound =
        crate::branch_context::read_issue_id(&git_runner, &repo.head_branch).expect("binding read");
    assert_eq!(
        bound,
        Some(245),
        "stale branch binding must survive acquire"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_dirty_foreign_bound_reuses_checkout_by_default() {
    // Issue 651 P2 acceptance criterion 1: with no conflicting active
    // lease on the current checkout path, acquire reuses it whether
    // the tree is dirty or bound to a historical issue. The foreign
    // binding is advisory context only and must survive untouched.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("dirty-foreign-reuse") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-dirty-foreign-reuse");
    let cache = unique_cache("dirty-foreign-reuse");
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
    .expect("dirty + foreign-bound must reuse, not isolate");
    assert!(
        !outcome.created,
        "a dirty foreign-bound checkout must be reused by default"
    );
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("dirty") && joined.contains("advisory only"),
        "the advisory dirty warning must survive: {joined}"
    );
    assert!(
        !cache.path().join("worktrees").exists(),
        "reuse must not create a worktree directory"
    );
    let git_runner = crate::branch_context::ProcessGitRunner::in_directory(repo.dir.path());
    let bound =
        crate::branch_context::read_issue_id(&git_runner, &repo.head_branch).expect("binding read");
    assert_eq!(bound, Some(241), "the foreign binding must survive acquire");
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_reuses_free_primary_despite_active_lease_on_linked_worktree() {
    // Issue 651 P2 acceptance criterion 2: an active lease on a
    // separate linked worktree does not force isolation of a free
    // primary checkout — the conflict decision is scoped to the
    // target checkout path.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("free-primary-linked") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-free-primary-linked");
    let cache = unique_cache("free-primary-linked");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    seed_active_lease_on_path(
        &identity,
        999,
        "other-session",
        &cache.path().join("linked-worktree").to_string_lossy(),
    );
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("a free primary must reuse despite a live lease elsewhere");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_same_session_different_issue_on_occupied_path_isolates() {
    // The occupant need not be foreign: reusing an occupied checkout
    // path would collide on the active-only `(repo_identity,
    // worktree_path)` index, so even the same session acquiring a
    // different issue isolates instead of sharing the path.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("same-session-occupied") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-same-session-occupied");
    let cache = unique_cache("same-session-occupied");
    let runner = ProcessWorktreeRunner::new();
    let first = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
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
        246,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("a second issue on the occupied path must isolate, not collide");
    assert!(second.created);
    assert_eq!(second.reason, "new_worktree");
    assert_ne!(second.path, first.path);
    assert!(second.branch.starts_with("phasegent/246-"));
    let joined = second.warnings.join(" ");
    assert!(
        joined.contains("session-A") && joined.contains("245") && joined.contains(&first.lease_id),
        "the warning must name the occupying session, issue, and lease: {joined}"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_reuse_reports_path_conflict_with_isolation_guidance() {
    // The `isolation`-kind error contract survives the lease-first
    // rewrite: a `reuse` caller facing an occupied checkout path
    // gets guidance naming the occupant and the explicit command,
    // writes no lease row, and creates no directory.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("reuse-only-path-conflict") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-reuse-only-path-conflict");
    let cache = unique_cache("reuse-only-path-conflict");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    seed_active_lease_on_path(
        &identity,
        245,
        "session-A",
        &repo.dir.path().to_string_lossy(),
    );
    let error = acquire_lease_with(
        &runner,
        repo.dir.path(),
        246,
        "session-B",
        AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: false,
            base: None,
            reuse: true,
        },
    )
    .expect_err("an occupied checkout path must refuse implicit creation");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("session-A")
            && error.message.contains("245")
            && error.message.contains("--isolate")
            && error.message.contains("issue 246"),
        "guidance must name the occupant and the explicit command: {error}"
    );
    let rows = leases_for_repo(&identity).expect("list leases");
    assert_eq!(
        rows.len(),
        1,
        "a refused conflict must not book the checkout: {rows:?}"
    );
    assert!(
        !cache.path().join("worktrees").exists(),
        "a refused conflict must not create a worktree directory"
    );
    drop(cache);
    drop(db_temp);
}
