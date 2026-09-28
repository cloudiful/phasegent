//! Acquire flows driven by durable branch links (issue 628 P4).
//!
//! A single unambiguous link decides dirty-tree ownership like the
//! legacy binding did; ambiguous links never reuse a dirty tree; and
//! a missing link store degrades to the legacy binding byte-for-byte.

use super::support::*;
use super::*;
use crate::worktree::AcquireOptions;

fn local_key(repo: &TempRepo) -> String {
    crate::branch_links::resolve_repo_key(None, repo.dir.path())
        .expect("fallback key")
        .key
}

fn seed_link(storage: &Storage, repo_key: &str, branch: &str, issue: u64) {
    crate::branch_links::ensure_schema(&storage.connection).expect("link schema");
    let key = crate::branch_links::IssueKey::from_number("redmine", "tools-phasegent", issue)
        .expect("issue key");
    crate::branch_links::store::link(
        &storage.connection,
        &crate::branch_links::LinkParams {
            repo_key,
            branch,
            issue: &key,
            issue_number: issue,
            source: "test",
            now: now_unix_secs().max(1),
        },
    )
    .expect("link must insert");
}

fn dirty(repo: &TempRepo) -> PathBuf {
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    scratch
}

fn reuse_only(cache: &TempDir) -> AcquireOptions<'_> {
    AcquireOptions {
        cache_base: Some(cache.path()),
        isolate: false,
        auto: false,
        base: None,
        reuse_only: true,
    }
}

#[test]
fn dirty_db_single_same_issue_reuses_checkout() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("links-reuse") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("links-reuse");
    seed_link(&storage, &local_key(&repo), &repo.head_branch, 245);
    drop(storage);
    let cache = unique_cache("links-reuse");
    let scratch = dirty(&repo);
    let outcome = crate::worktree::acquire_lease_with(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        245,
        "session-A",
        reuse_only(&cache),
    )
    .expect("dirty tree owned by this issue must reuse");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert!(
        outcome.warnings.is_empty(),
        "owned dirty reuse stays quiet: {:?}",
        outcome.warnings
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn dirty_db_single_foreign_issue_isolates() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("links-foreign") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("links-foreign");
    seed_link(&storage, &local_key(&repo), &repo.head_branch, 241);
    drop(storage);
    let cache = unique_cache("links-foreign");
    let scratch = dirty(&repo);
    let error = crate::worktree::acquire_lease_with(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        245,
        "session-A",
        reuse_only(&cache),
    )
    .expect_err("dirty tree owned by another issue must not reuse");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("241"),
        "the owner must be named: {error}"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn dirty_db_ambiguous_links_never_reuse() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("links-ambiguous") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("links-ambiguous");
    seed_link(&storage, &local_key(&repo), &repo.head_branch, 241);
    seed_link(&storage, &local_key(&repo), &repo.head_branch, 242);
    drop(storage);
    let cache = unique_cache("links-ambiguous");
    let scratch = dirty(&repo);
    let error = crate::worktree::acquire_lease_with(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        241,
        "session-A",
        reuse_only(&cache),
    )
    .expect_err("ambiguous ownership must not reuse, even for a linked issue");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("multiple issues"),
        "ambiguity must be explained: {error}"
    );
    let outcome = acquire_lease(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        241,
        "session-A",
        Some(cache.path()),
        true,
        false,
    )
    .expect("explicit isolation still creates");
    assert!(outcome.created);
    assert_eq!(outcome.reason, "new_worktree");
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("multiple issues")),
        "isolation trigger belongs in warnings: {:?}",
        outcome.warnings
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn durable_link_beats_a_stale_legacy_binding() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("links-authoritative") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("links-authoritative");
    // Legacy says 241, the durable store says 245: the store wins.
    seed_link(&storage, &local_key(&repo), &repo.head_branch, 245);
    drop(storage);
    bind_current_branch(&repo, 241);
    let cache = unique_cache("links-authoritative");
    let scratch = dirty(&repo);
    let outcome = crate::worktree::acquire_lease_with(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        245,
        "session-A",
        reuse_only(&cache),
    )
    .expect("durable ownership must reuse");
    assert_eq!(outcome.reason, "no_conflict");
    let error = crate::worktree::acquire_lease_with(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        241,
        "session-A",
        reuse_only(&cache),
    )
    .expect_err("legacy agreement must not override the store");
    assert_eq!(error.kind, "isolation");
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn missing_link_store_keeps_legacy_behavior() {
    // No `branch_issue_links` table at all (pre-P3 database): the
    // legacy binding decides exactly as before.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("links-missing-table") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("links-missing-table");
    crate::worktree::ensure_schema(&storage).expect("lease schema only");
    drop(storage);
    bind_current_branch(&repo, 241);
    let cache = unique_cache("links-missing-table");
    let scratch = dirty(&repo);
    let outcome = acquire_lease(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        241,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("legacy same-issue ownership must reuse");
    assert!(
        !outcome.created,
        "legacy path must not isolate: {outcome:?}"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn clean_checkout_with_links_reuses_regardless() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("links-clean") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("links-clean");
    // Even ambiguous links do not disturb a clean checkout: reuse is
    // safe and the booked lease names the requested issue explicitly.
    seed_link(&storage, &local_key(&repo), &repo.head_branch, 241);
    seed_link(&storage, &local_key(&repo), &repo.head_branch, 242);
    drop(storage);
    let cache = unique_cache("links-clean");
    let outcome = acquire_lease(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        241,
        "session-A",
        Some(cache.path()),
        false,
        false,
    )
    .expect("clean checkout must reuse");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    drop(cache);
    drop(db_temp);
}
