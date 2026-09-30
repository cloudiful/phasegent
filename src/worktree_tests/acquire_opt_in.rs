//! Reuse-preference acquisition (issue 651 P3).
//!
//! The coherent reuse/isolate pair replaced the boolean `worktree-auto`
//! switch and the implicit `reuse_only` gate: `reuse` states the reuse
//! preference explicitly (or implicitly, for the create/bind hook),
//! `--isolate` forces a fresh worktree, and passing both is a
//! structured `argument` error. A `reuse` caller never creates: an
//! occupied checkout path resolves to `isolation` guidance instead,
//! while dirt, bindings, and leases on other paths reuse exactly like
//! the default table.

use super::support::*;
use super::*;

use crate::worktree::{AcquireOptions, acquire_lease_with};

/// Seed one active lease row for `identity` directly in the temp DB so the
/// decision table sees a foreign or parallel session.
fn seed_active_lease(identity: &str, issue: u64, session: &str, worktree_path: &str) {
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
            branch: "main",
            status: LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("seed lease");
}

fn reuse_options<'a>(cache_base: &'a Path) -> AcquireOptions<'a> {
    AcquireOptions {
        cache_base: Some(cache_base),
        isolate: false,
        base: None,
        reuse: true,
    }
}

#[test]
fn reuse_prefers_the_current_checkout_and_records_the_lease() {
    // The reuse preference keeps safe reuse and automatic lease
    // bookkeeping: only directory creation is off the table.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-reuse") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-reuse");
    let cache = unique_cache("opt-in-reuse");
    let runner = ProcessWorktreeRunner::new();
    let outcome = acquire_lease_with(
        &runner,
        repo.dir.path(),
        616,
        "session-A",
        reuse_options(cache.path()),
    )
    .expect("a safe checkout must still be reused");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    assert!(
        outcome.warnings.is_empty(),
        "safe reuse must stay silent: {:?}",
        outcome.warnings
    );
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let rows = leases_for_repo(&identity).expect("list leases");
    assert_eq!(
        rows.len(),
        1,
        "the reuse must still record one active lease"
    );
    assert_eq!(rows[0].issue, 616);
    assert_eq!(rows[0].session, "session-A");
    assert!(
        !cache.path().join("worktrees").exists(),
        "reuse must not create a worktree directory"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_ignores_dirt_with_an_advisory_warning() {
    // Dirt is advisory only: a dirty checkout reuses under the reuse
    // preference and warns about the dirt.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-dirty-foreign") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-dirty-foreign");
    let cache = unique_cache("opt-in-dirty-foreign");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    let outcome = acquire_lease_with(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        reuse_options(cache.path()),
    )
    .expect("a dirty checkout must reuse under the reuse preference");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    let joined = outcome.warnings.join(" ");
    assert!(
        joined.contains("dirty") && joined.contains("advisory only"),
        "the advisory dirty warning must survive: {joined}"
    );
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let rows = leases_for_repo(&identity).expect("list leases");
    assert_eq!(
        rows.len(),
        1,
        "the reuse must still book the checkout: {rows:?}"
    );
    assert!(
        !cache.path().join("worktrees").exists(),
        "reuse must not create a worktree directory"
    );
    assert!(
        repo.dir.path().exists(),
        "reuse must never delete the current checkout"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_refuses_an_occupied_path_with_guidance() {
    // The reuse preference never bypasses an active lease on the
    // checkout path itself: the occupant is named, the explicit
    // `--isolate` command is given, and nothing is booked or created.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-occupied") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-occupied");
    let cache = unique_cache("opt-in-occupied");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    seed_active_lease(
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
        reuse_options(cache.path()),
    )
    .expect_err("an occupied checkout path must refuse the reuse preference");
    assert_eq!(error.kind, "isolation");
    assert!(
        error.message.contains("session-A")
            && error.message.contains("245")
            && error.message.contains("--isolate")
            && error.message.contains("issue 246"),
        "guidance must name the occupant and the explicit command: {error}"
    );
    assert!(
        error.message.chars().count() <= 200,
        "the bounded message must keep the whole command: {error}"
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
    assert!(
        repo.dir.path().exists(),
        "a refused conflict must never delete the current checkout"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_reuses_despite_a_foreign_lease_elsewhere() {
    // Path scope applies to the reuse preference too: an active lease
    // on another checkout never blocks a free primary.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-foreign-lease") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-foreign-lease");
    let cache = unique_cache("opt-in-foreign-lease");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    seed_active_lease(&identity, 999, "other-session", "/tmp/phasegent-foreign");
    let outcome = acquire_lease_with(
        &runner,
        repo.dir.path(),
        616,
        "session-A",
        reuse_options(cache.path()),
    )
    .expect("a foreign lease elsewhere must not block reuse");
    assert!(!outcome.created);
    assert_eq!(outcome.reason, "no_conflict");
    assert_eq!(outcome.path, repo.dir.path().to_string_lossy().to_string());
    let rows = leases_for_repo(&identity).expect("list leases");
    assert_eq!(rows.len(), 2, "both leases stay live: {rows:?}");
    assert!(
        !cache.path().join("worktrees").exists(),
        "reuse must not create a worktree directory"
    );
    drop(cache);
    drop(db_temp);
}

#[test]
fn isolate_and_reuse_together_are_rejected() {
    // Forcing a fresh worktree while stating the reuse preference is
    // contradictory: the table fails fast with a structured `argument`
    // error before any storage or git work, mirroring the CLI parser's
    // mutual-exclusion rejection.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-contradiction") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-contradiction");
    let cache = unique_cache("opt-in-contradiction");
    let runner = ProcessWorktreeRunner::new();
    let scratch = repo.dir.path().join("scratch.txt");
    std::fs::write(&scratch, "scratch\n").expect("write scratch");
    bind_current_branch(&repo, 241);
    let error = acquire_lease_with(
        &runner,
        repo.dir.path(),
        245,
        "session-A",
        AcquireOptions {
            cache_base: Some(cache.path()),
            isolate: true,
            base: None,
            reuse: true,
        },
    )
    .expect_err("isolate + reuse must fail fast");
    assert_eq!(error.kind, "argument");
    assert!(
        error.message.contains("--isolate")
            && error.message.contains("--reuse")
            && error.message.contains("mutually exclusive"),
        "the rejection must name both spellings: {error}"
    );
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let rows = leases_for_repo(&identity).expect("list leases");
    assert!(
        rows.is_empty(),
        "a rejected combination must not book the checkout: {rows:?}"
    );
    assert!(
        !cache.path().join("worktrees").exists(),
        "a rejected combination must not create a worktree directory"
    );
    let _ = std::fs::remove_file(&scratch);
    drop(cache);
    drop(db_temp);
}

#[test]
fn reuse_keeps_the_idempotent_home_coming() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("opt-in-idempotent") else {
        return;
    };
    let (db_temp, _storage, _env) = open_temp_db("acquire-opt-in-idempotent");
    let cache = unique_cache("opt-in-idempotent");
    let runner = ProcessWorktreeRunner::new();
    let first = acquire_lease_with(
        &runner,
        repo.dir.path(),
        616,
        "session-A",
        reuse_options(cache.path()),
    )
    .expect("first reuse");
    let second = acquire_lease_with(
        &runner,
        repo.dir.path(),
        616,
        "session-A",
        reuse_options(cache.path()),
    )
    .expect("second acquire must hit the idempotent home-coming");
    assert_eq!(second.reason, "idempotent");
    assert_eq!(first.lease_id, second.lease_id);
    assert_eq!(first.path, second.path);
    assert!(
        second.warnings.is_empty(),
        "idempotent reuse must stay silent: {:?}",
        second.warnings
    );
    drop(cache);
    drop(db_temp);
}
