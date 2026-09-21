use super::support::*;
use super::*;

#[test]
fn issue_sync_no_clean_reports_verdicts_without_writing() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-report");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let clean_number = sync_cli_seed_closed_issue("Report clean");
    let clean_worktree =
        close_cli_add_worktree(&repo, "sync-report-clean", "feat/552-report-clean");
    let dirty_number = sync_cli_seed_closed_issue("Report dirty");
    let dirty_worktree =
        close_cli_add_worktree(&repo, "sync-report-dirty", "feat/552-report-dirty");
    fs::write(dirty_worktree.join("scratch.txt"), "wip").unwrap();
    let identity = sync_cli_identity(&repo);
    let clean_lease = close_cli_seed_lease_at(
        &identity,
        clean_number,
        "session-report",
        "active",
        &clean_worktree,
    );
    let dirty_lease = close_cli_seed_lease_at(
        &identity,
        dirty_number,
        "session-dirty",
        "active",
        &dirty_worktree,
    );
    let clean_dir = clean_worktree.to_string_lossy().to_string();
    let dirty_dir = dirty_worktree.to_string_lossy().to_string();

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Report);

    assert_eq!(report.mode, "report");
    assert_eq!(report.checked, 2);
    assert_eq!(report.released_leases, 0);
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.kept, 1);
    let clean_issue = report
        .issues
        .iter()
        .find(|issue| issue.issue == clean_number)
        .expect("closed clean issue is reported");
    assert_eq!(clean_issue.active_leases, 1);
    assert_eq!(clean_issue.released_leases, 0);
    assert_eq!(clean_issue.cleaned, 0);
    let clean_report = clean_issue
        .directories
        .iter()
        .find(|directory| directory.path == clean_dir)
        .expect("clean directory verdict");
    assert_eq!(clean_report.action, "would_clean");
    assert!(clean_report.reason.is_none());
    let dirty_issue = report
        .issues
        .iter()
        .find(|issue| issue.issue == dirty_number)
        .expect("closed dirty issue is reported");
    let dirty_report = dirty_issue
        .directories
        .iter()
        .find(|directory| directory.path == dirty_dir)
        .expect("dirty directory verdict");
    assert_eq!(dirty_report.action, "would_keep");
    assert!(
        dirty_report
            .reason
            .as_deref()
            .expect("keep reason")
            .contains("uncommitted or untracked files"),
        "got: {:?}",
        dirty_report.reason
    );

    // Report mode writes nothing, including the lease convergence.
    assert!(clean_worktree.exists());
    assert!(dirty_worktree.exists());
    assert_eq!(close_cli_lease_state(&clean_lease).0, "active");
    assert_eq!(close_cli_lease_state(&dirty_lease).0, "active");

    // `issue sync --no-clean` exits 0 and still writes nothing.
    let exit = sync_cli_run_cli(ProviderKind::Local, false, true, &repo);
    assert_eq!(exit, 0);
    assert!(clean_worktree.exists(), "report mode must not clean");
    assert_eq!(close_cli_lease_state(&clean_lease).0, "active");
    let _ = fs::remove_dir_all(&clean_worktree);
    let _ = fs::remove_dir_all(&dirty_worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_all_scans_every_lease_repository() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-all");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo_a = root.join("repo-a");
    close_cli_init_repo(&repo_a);
    let worktree_a = close_cli_add_worktree(&repo_a, "sync-all-a", "feat/552-all-a");
    let number_a = sync_cli_seed_closed_issue("All scan A");
    let identity_a = sync_cli_identity(&repo_a);
    let lease_a =
        close_cli_seed_lease_at(&identity_a, number_a, "session-a", "active", &worktree_a);

    let repo_b = root.join("repo-b");
    close_cli_init_repo(&repo_b);
    let worktree_b = close_cli_add_worktree(&repo_b, "sync-all-b", "feat/552-all-b");
    let number_b = sync_cli_seed_closed_issue("All scan B");
    let identity_b = sync_cli_identity(&repo_b);
    let lease_b =
        close_cli_seed_lease_at(&identity_b, number_b, "session-b", "active", &worktree_b);

    // A lease whose checkout is gone must be reported, not scanned.
    let missing_identity = format!(
        "{}/phasegent-sync-missing-{}/.git",
        crate::test_scratch::root().display(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    close_cli_seed_lease_at(
        &missing_identity,
        9_000,
        "session-missing",
        "active",
        &std::path::Path::new(&missing_identity).with_file_name("worktree"),
    );

    let report = sync_cli_pass(&repo_a, true, crate::cli::issue::sync::SyncMode::Clean);

    assert!(report.all);
    assert_eq!(report.checked, 2);
    assert_eq!(report.released_leases, 2);
    assert_eq!(report.cleaned, 2);
    assert_eq!(report.skipped_repos.len(), 1);
    assert!(
        report.skipped_repos[0].repo_identity == missing_identity,
        "the missing checkout is reported verbatim"
    );
    assert!(
        report.skipped_repos[0].reason.contains("missing"),
        "got: {}",
        report.skipped_repos[0].reason
    );
    let cleaned: Vec<u64> = report.issues.iter().map(|issue| issue.issue).collect();
    assert!(cleaned.contains(&number_a) && cleaned.contains(&number_b));
    assert!(!worktree_a.exists(), "repo A's residue is reconciled");
    assert!(!worktree_b.exists(), "repo B's residue is reconciled");
    assert_eq!(close_cli_lease_state(&lease_a).0, "retained");
    assert_eq!(close_cli_lease_state(&lease_b).0, "retained");

    // The CLI wiring accepts `--all` and exits 0 on the converged state.
    let exit = sync_cli_run_cli(ProviderKind::Local, true, false, &repo_a);
    assert_eq!(exit, 0);
    let _ = fs::remove_dir_all(&worktree_a);
    let _ = fs::remove_dir_all(&worktree_b);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_skips_missing_remote_issue() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-missing-remote");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-missing-remote", "feat/552-missing");
    let identity = sync_cli_identity(&repo);
    // No provider issue 9_999 exists for this lease row: a stale row must
    // not wedge the pass.
    let lease = close_cli_seed_lease_at(&identity, 9_999, "session-missing", "active", &worktree);

    let report = sync_cli_pass(&repo, false, crate::cli::issue::sync::SyncMode::Clean);

    assert_eq!(report.checked, 1);
    assert_eq!(report.not_found, 1);
    assert!(report.issues.is_empty());
    assert!(worktree.exists(), "a missing remote issue deletes nothing");
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_reports_remote_failure_with_non_zero_exit() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("sync-remote-failure");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );
    let api_base = sync_cli_dead_api_base();
    let _api_guard = EnvGuard::set("PHASEGENT_API_BASE", &api_base);
    let _repo_guard = EnvGuard::set("PHASEGENT_REPOSITORY", "owner/repo");
    Storage::open()
        .expect("storage")
        .save_credential(Role::Orchestrator, "forgejo", "sync-test-token")
        .expect("store forgejo token");

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "sync-remote", "feat/552-remote");
    let number = sync_cli_seed_closed_issue("Remote down");
    let identity = sync_cli_identity(&repo);
    let lease = close_cli_seed_lease_at(&identity, number, "session-remote", "active", &worktree);

    let exit = sync_cli_run_cli(ProviderKind::Forgejo, false, false, &repo);

    assert_ne!(
        exit, 0,
        "a direct sync against an unreachable remote must exit non-zero"
    );
    assert!(
        worktree.exists(),
        "a failed pass must not delete a worktree directory"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn issue_sync_rejects_non_orchestrator_roles() {
    // The role gate fires before any storage or provider access.
    for role in [Role::Executor, Role::Reviewer, Role::Tester, Role::Admin] {
        let exit = crate::cli::issue::execute_issue(
            Some(role),
            Some(ProviderKind::Local),
            None,
            None,
            None,
            None,
            command::IssueCommand::Sync {
                all: false,
                no_clean: false,
            },
        );
        assert_eq!(exit, 3, "role '{role}' must be denied with exit code 3");
    }
}
