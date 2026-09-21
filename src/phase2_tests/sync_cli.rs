use super::support::*;
use super::*;

#[test]
fn issue_sync_cleans_remotely_closed_issue_residue() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-clean");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-clean", "feat/552-sync");
    let number = sync_cli_seed_closed_issue("Sync me");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-sync", "active", &worktree);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.mode, "clean");
    assert!(!report.all);
    assert_eq!(report.checked, 1);
    assert_eq!(report.not_closed, 0);
    assert_eq!(report.released_leases, 1);
    assert_eq!(report.cleaned, 1);
    assert_eq!(report.kept, 0);
    assert_eq!(report.issues.len(), 1);
    let issue = &report.issues[0];
    assert_eq!(issue.issue, number);
    assert_eq!(issue.remote_state, "closed");
    assert_eq!(issue.active_leases, 1);
    assert_eq!(issue.released_leases, 1);
    assert_eq!(issue.directories.len(), 1);
    assert_eq!(issue.directories[0].action, "cleaned");
    assert!(issue.directories[0].reason.is_none());

    assert!(
        !worktree.exists(),
        "a remotely closed issue's clean worktree must be removed"
    );
    let (status, reason) = close_cli_lease_state(&lease);
    assert_eq!(status, "retained");
    assert_eq!(
        reason.as_deref(),
        Some("issue closed on the remote (issue sync)")
    );
    assert!(
        close_cli_branch_exists(&repo, "feat/552-sync"),
        "the branch must never be deleted"
    );

    // The CLI wiring exits 0 for the same converged state (nothing left to
    // reconcile once the directory is gone).
    let exit = sync_cli_run_cli(ProviderKind::Local, false, false, &repo);
    assert_eq!(exit, 0);
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_keeps_dirty_worktree_and_reports_reason() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-dirty");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-dirty", "feat/552-dirty");
    // An untracked file is enough to make the directory dirty.
    fs::write(worktree.join("scratch.txt"), "wip").unwrap();
    let number = sync_cli_seed_closed_issue("Sync dirty");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-dirty", "active", &worktree);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.cleaned, 0);
    assert_eq!(report.kept, 1);
    let directory = &report.issues[0].directories[0];
    assert_eq!(directory.action, "kept");
    let reason = directory.reason.as_deref().expect("keep reason");
    assert!(
        reason.contains("uncommitted or untracked files"),
        "the shared cleanliness guard must supply the reason; got: {reason}"
    );
    assert!(worktree.exists(), "a dirty worktree must be kept");
    // The lease still converges: the issue is closed remotely.
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_never_removes_the_main_checkout() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-main-checkout");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let number = sync_cli_seed_closed_issue("Sync main");
    let identity = sync_cli_identity(&repo);
    // A reused-current-checkout lease points at the main checkout itself.
    let lease = close_cli_seed_lease_at(&identity, number, "session-main", "active", &repo);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.cleaned, 0);
    assert_eq!(report.kept, 1);
    let directory = &report.issues[0].directories[0];
    assert_eq!(directory.action, "kept");
    let reason = directory.reason.as_deref().expect("keep reason");
    assert!(
        reason.contains("main checkout is never removed"),
        "the shared main-checkout guard must supply the reason; got: {reason}"
    );
    assert!(repo.exists(), "the main checkout is never removed");
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn issue_sync_keeps_directory_held_by_another_issues_active_lease() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-foreign-lease");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-foreign", "feat/552-foreign");
    let number = sync_cli_seed_closed_issue("Sync foreign");
    let other_issue = close_cli_seed_issue("Still open", "New");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-sync", "active", &worktree);
    // Another issue holds an active lease on the same directory through a
    // symlinked spelling: the unique `(repo, path)` lease index keeps the
    // row distinct while the shared directory guard still sees one target.
    let link = root.join("foreign-link");
    std::os::unix::fs::symlink(&worktree, &link).expect("symlink");
    let other_lease =
        close_cli_seed_lease_at(&identity, other_issue, "session-foreign", "active", &link);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.checked, 2);
    assert_eq!(report.not_closed, 1);
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.kept, 1);
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.issue == number)
        .expect("the closed issue is reported");
    assert_eq!(issue.directories[0].action, "kept");
    let reason = issue.directories[0].reason.as_deref().expect("keep reason");
    assert!(
        reason.contains("session-foreign") && reason.contains(&format!("issue {other_issue}")),
        "the shared foreign-lease guard must name the owning session and issue; got: {reason}"
    );
    assert!(worktree.exists(), "the foreign lease keeps the directory");
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    assert_eq!(
        close_cli_lease_state(&other_lease).0,
        "active",
        "another issue's active lease is never released"
    );
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_leaves_open_issue_residue_and_lease_untouched() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-open");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let open_issue = close_cli_seed_issue("Still open", "New");
    let worktree = close_cli_add_worktree(&repo, "sync-open", "feat/552-open");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, open_issue, "session-open", "active", &worktree);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.checked, 1);
    assert_eq!(report.not_closed, 1);
    assert!(
        report.issues.is_empty(),
        "an open issue is not a reconciliation candidate"
    );
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.released_leases, 0);
    assert!(worktree.exists());
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}
