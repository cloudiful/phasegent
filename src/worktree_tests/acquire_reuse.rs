use super::support::*;
use super::*;

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
    // Dirty + unbound: no task evidence for the dirt, so reuse the
    // checkout but surface a best-effort warning.
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
        joined.contains("dirty") && joined.contains("not bound"),
        "reuse of a dirty unbound checkout must carry a warning: {joined}"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn acquire_dirty_same_issue_other_session_lease_creates_new_worktree() {
    // Two parallel sessions on the same dirty checkout bound to the
    // same issue must not share the tree: once session A has recorded
    // a lease, session B gets its own worktree.
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
    // Issue #509: explicit `--isolate` now forces a fresh worktree on every
    // path, so this Rule 3 fall-through/reuse coverage uses the default
    // path (no flag).
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
    // No lease exists yet and the dirt belongs to our own issue: the
    // crashed-predecessor fall-through reuses the checkout.
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
        "parallel session must not share a dirty tree"
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
    // and must keep the stale branch binding as safe evidence instead of
    // auto-clearing it.
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
    // Issue #509: explicit `--isolate` now forces a fresh worktree, so this
    // crashed-predecessor reuse coverage uses the default path (no flag).
    let outcome = acquire_lease(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("dirty + same-issue with no other lease must fall through to reuse");
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
