//! Sequential leasing on one checkout through the CLI (issue 628
//! P4): a terminal (retained/released) lease never blocks a later
//! active lease for the same `(repo_identity, worktree_path)`, while
//! two live leases still conflict. History rows stay as audit
//! records.

use super::cli_support::*;
use super::support::*;
use super::*;
use crate::worktree::release_lease;

fn acquire(repo: &TempRepo, issue: u64, session: &str) -> i32 {
    in_temp_repo(repo, || {
        execute_worktree(
            Some(Role::Orchestrator),
            WorktreeCommand::Acquire {
                issue,
                session: Some(session.to_owned()),
                base: None,
                format: "json".to_owned(),
                isolate: false,
                no_sync: true,
            },
        )
    })
}

fn active_lease_id(storage: &Storage, identity: &str) -> String {
    list_for_repo(storage, identity)
        .expect("repo list")
        .into_iter()
        .find(|row| row.status == LEASE_STATUS_ACTIVE)
        .expect("one active lease")
        .lease_id
}

#[test]
fn cli_acquire_reuses_checkout_after_terminal_lease() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-reuse-terminal") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-reuse-terminal");
    assert_eq!(acquire(&repo, 245, "session-A"), 0, "first acquire reuses");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let storage = Storage::open().expect("temp storage");
    let first = active_lease_id(&storage, &identity);
    release_lease(&first, true).expect("release to retained");
    drop(storage);
    // Pre-P4 this second acquire failed on the full-table unique
    // index; now the retained row is history and reuse succeeds.
    assert_eq!(
        acquire(&repo, 245, "session-A"),
        0,
        "retained history must not block re-acquire"
    );
    let storage = Storage::open().expect("temp storage");
    let rows = list_for_repo(&storage, &identity).expect("repo list");
    assert_eq!(rows.len(), 2, "history plus the new live row stay");
    assert!(
        rows.iter()
            .any(|row| row.status == LEASE_STATUS_RETAINED && row.lease_id == first),
        "the released row is preserved: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| {
            row.status == LEASE_STATUS_ACTIVE && row.issue == 245 && row.lease_id != first
        }),
        "a fresh active row owns the checkout: {rows:?}"
    );
    drop(db_temp);
}

#[test]
fn cli_sequential_issues_share_checkout_after_release() {
    // The P1 primary-checkout pattern (#337 retained, #600 next):
    // closing an issue converges its lease, and the next issue
    // reuses the same path without an isolation error.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-sequential") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-sequential");
    assert_eq!(acquire(&repo, 245, "session-A"), 0);
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let storage = Storage::open().expect("temp storage");
    let first = active_lease_id(&storage, &identity);
    let first_path = list_for_repo(&storage, &identity)
        .expect("repo list")
        .into_iter()
        .find(|row| row.lease_id == first)
        .expect("first row")
        .worktree_path;
    release_lease(&first, true).expect("release to retained");
    drop(storage);
    assert_eq!(
        acquire(&repo, 246, "session-A"),
        0,
        "the next issue must reuse the released checkout"
    );
    let storage = Storage::open().expect("temp storage");
    let rows = list_for_repo(&storage, &identity).expect("repo list");
    let live = rows
        .iter()
        .find(|row| row.status == LEASE_STATUS_ACTIVE)
        .expect("one live row");
    assert_eq!(live.issue, 246);
    assert_eq!(
        live.worktree_path, first_path,
        "sequential issues share the primary checkout"
    );
    drop(db_temp);
}

#[test]
fn cli_simultaneous_active_checkout_still_conflicts() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-still-conflict") else {
        return;
    };
    let (db_temp, _cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-still-conflict");
    assert_eq!(acquire(&repo, 245, "session-A"), 0);
    // A second live triple for the same checkout cannot reuse it and
    // must not create implicitly either: `--isolate` was not passed,
    // but the explicit `worktree acquire` may create, so it isolates.
    let exit = in_temp_repo(&repo, || {
        execute_worktree(
            Some(Role::Orchestrator),
            WorktreeCommand::Acquire {
                issue: 245,
                session: Some("session-B".to_owned()),
                base: None,
                format: "json".to_owned(),
                isolate: false,
                no_sync: true,
            },
        )
    });
    assert_eq!(exit, 0, "explicit acquire isolates a live conflict");
    let identity = repo_identity(&ProcessWorktreeRunner::new(), repo.dir.path()).expect("identity");
    let storage = Storage::open().expect("temp storage");
    let actives = list_for_repo(&storage, &identity)
        .expect("repo list")
        .into_iter()
        .filter(|row| row.status == LEASE_STATUS_ACTIVE)
        .collect::<Vec<_>>();
    assert_eq!(actives.len(), 2, "both sessions hold live leases");
    assert!(
        actives.iter().all(|row| row.issue == 245),
        "parallel sessions share the issue, not the checkout: {actives:?}"
    );
    assert_ne!(
        actives[0].worktree_path, actives[1].worktree_path,
        "live leases never share a path"
    );
    drop(db_temp);
}

#[test]
fn cli_acquire_reuses_free_primary_despite_active_lease_elsewhere() {
    // Issue 651 P2 acceptance criterion 2 through the CLI: an active
    // lease on a linked worktree path does not force isolation of a
    // free primary checkout. The acquire exits 0 and books the primary
    // path while the foreign lease stays untouched.
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cli-free-primary") else {
        return;
    };
    let (db_temp, cache_temp, _db_env, _cache_env) = open_temp_db_and_cache("cli-free-primary");
    let runner = ProcessWorktreeRunner::new();
    let identity = repo_identity(&runner, repo.dir.path()).expect("identity");
    let storage = Storage::open().expect("temp storage");
    ensure_schema(&storage).expect("schema");
    insert_lease_for_identity(
        &storage,
        "lease-linked",
        &identity,
        "other-session",
        LEASE_STATUS_ACTIVE,
        now_unix_secs(),
        &cache_temp.path().join("linked-worktree").to_string_lossy(),
    );
    drop(storage);
    assert_eq!(
        acquire(&repo, 245, "session-A"),
        0,
        "a free primary must reuse despite a live lease elsewhere"
    );
    let storage = Storage::open().expect("temp storage");
    let rows = list_for_repo(&storage, &identity).expect("repo list");
    assert_eq!(rows.len(), 2, "both the foreign and the new lease stay");
    let live = rows
        .iter()
        .find(|row| row.status == LEASE_STATUS_ACTIVE && row.session == "session-A")
        .expect("new live row");
    assert_eq!(live.issue, 245);
    assert_eq!(
        live.worktree_path,
        repo.dir.path().to_string_lossy().to_string(),
        "the new lease books the primary checkout, not a worktree"
    );
    assert!(
        !cache_temp.path().join("worktrees").exists(),
        "reuse must not create a worktree directory"
    );
    drop(db_temp);
}
