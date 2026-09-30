//! Acquire flows in the presence of durable branch links (issue 628
//! P4, lease-first since issue 651 P2).
//!
//! Links never decide: single, foreign, ambiguous, and missing-store
//! states all reuse a free checkout, and only an active lease on the
//! checkout path (or an explicit `--isolate`) creates a worktree.

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

fn reuse_options(cache: &TempDir) -> AcquireOptions<'_> {
    AcquireOptions {
        cache_base: Some(cache.path()),
        isolate: false,
        base: None,
        reuse: true,
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
        reuse_options(&cache),
    )
    .expect("dirty tree with a same-issue link must reuse");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("dirty") && joined.contains("advisory only"),
        "owned dirty reuse carries only the advisory warning: {:?}",
        outcome.warnings
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn dirty_db_single_foreign_issue_reuses_checkout() {
    // Issue 651 P2: a durable link to another issue is advisory
    // context, not an isolation trigger. With no active lease on the
    // checkout path the dirty tree reuses.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("links-foreign") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("links-foreign");
    seed_link(&storage, &local_key(&repo), &repo.head_branch, 241);
    drop(storage);
    let cache = unique_cache("links-foreign");
    let scratch = dirty(&repo);
    let outcome = crate::worktree::acquire_lease_with(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        245,
        "session-A",
        reuse_options(&cache),
    )
    .expect("a foreign link must not block reuse");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn dirty_db_ambiguous_links_reuse_checkout() {
    // Ambiguous links are never guessed — and never isolate either.
    // Reuse wins on a free checkout; explicit isolation still creates
    // (with no link detail in its warnings, since links are not
    // consulted).
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
    let outcome = crate::worktree::acquire_lease_with(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        241,
        "session-A",
        reuse_options(&cache),
    )
    .expect("ambiguous links must not block reuse");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    // A different session forcing isolation still creates: the first
    // acquire booked the path, so this exercises the occupant warning
    // (with no link detail, since links are not consulted).
    let outcome = acquire_lease(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        241,
        "session-B",
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
            .all(|w| !w.contains("multiple issues")),
        "links are not consulted, so no link detail appears: {:?}",
        outcome.warnings
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn durable_links_are_advisory_only() {
    // The durable store never decides: a link to 245 while acquiring 241
    // reuses the same free checkout, and vice versa.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("links-authoritative") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("links-authoritative");
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
        reuse_options(&cache),
    )
    .expect("durable same-issue link must reuse");
    assert_eq!(outcome.reason, "no_conflict");
    // The first acquire booked the checkout path, so release it before
    // proving a second linked issue reuses the same free checkout.
    release_lease(&outcome.lease_id, true).expect("release first lease");
    let outcome = crate::worktree::acquire_lease_with(
        &ProcessWorktreeRunner::new(),
        repo.dir.path(),
        241,
        "session-B",
        reuse_options(&cache),
    )
    .expect("a link to another issue must not force isolation either");
    assert_eq!(outcome.reason, "no_conflict");
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn missing_link_store_keeps_reuse() {
    // No `branch_issue_links` table at all (pre-P3 database): the free
    // checkout still reuses. Links are never required for the reuse
    // path, so a missing store changes nothing.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("links-missing-table") else {
        return;
    };
    let (db_temp, storage, _env) = open_temp_db("links-missing-table");
    crate::worktree::ensure_schema(&storage).expect("lease schema only");
    drop(storage);
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
    .expect("a missing link store must not block reuse");
    assert!(!outcome.created, "reuse must not isolate: {outcome:?}");
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
