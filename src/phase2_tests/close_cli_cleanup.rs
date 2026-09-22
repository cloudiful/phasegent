use super::support::*;
use super::*;

/// Run one `issue close` against the local provider from `cwd`, with the
/// temp worktree database active, and return the exit code.
fn close_cli_run(number: u64, session: Option<&str>, cwd: &std::path::Path) -> i32 {
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(cwd).unwrap();
    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
        None,
        None,
        command::IssueCommand::Close {
            number,
            worktree_session: session.map(str::to_owned),
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);
    exit
}

#[test]
fn cli_issue_close_removes_clean_worktree_and_keeps_branch() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-clean");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "clean", "feat/552-clean");
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease = close_cli_seed_lease_at(&identity, number, "session-a", "active", &worktree);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_eq!(exit, 0, "the local-provider close must succeed");
    assert!(
        !worktree.exists(),
        "the clean worktree directory must be removed after the close"
    );
    assert_eq!(
        close_cli_lease_state(&lease).0,
        "retained",
        "the lease row stays as the retained audit record"
    );
    assert!(
        close_cli_branch_exists(&repo, "feat/552-clean"),
        "the branch must never be deleted"
    );
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_keeps_dirty_worktree() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-dirty");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "dirty", "feat/552-dirty");
    // An untracked file is enough to make the directory dirty.
    fs::write(worktree.join("scratch.txt"), "wip").unwrap();
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease = close_cli_seed_lease_at(&identity, number, "session-a", "active", &worktree);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_eq!(exit, 0);
    assert!(
        worktree.exists(),
        "a dirty worktree directory must be kept and warned about"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_keeps_another_issues_worktree() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-other-issue");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let ours = close_cli_add_worktree(&repo, "ours", "feat/552-ours");
    let theirs = close_cli_add_worktree(&repo, "theirs", "feat/9002-theirs");
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    close_cli_seed_lease_at(&identity, number, "session-a", "active", &ours);
    let other = close_cli_seed_lease_at(&identity, 9002, "session-b", "active", &theirs);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_eq!(exit, 0);
    assert!(
        !ours.exists(),
        "the closed issue's clean directory is removed"
    );
    assert!(
        theirs.exists(),
        "another issue's worktree directory is never touched"
    );
    assert_eq!(
        close_cli_lease_state(&other).0,
        "active",
        "another issue's active lease is never released"
    );
    let _ = fs::remove_dir_all(&ours);
    let _ = fs::remove_dir_all(&theirs);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_never_removes_the_main_checkout() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-main");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    // A reused-current-checkout lease points at the main checkout itself.
    let lease = close_cli_seed_lease_at(&identity, number, "session-a", "active", &repo);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_eq!(exit, 0);
    assert!(
        repo.exists(),
        "the main checkout is never removed by the close cleanup"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_provider_failure_leaves_worktree_directory() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-provider-failure");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "failure", "feat/552-failure");
    // `New -> Closed` is rejected by the local transition policy, so the
    // provider close fails before the cleanup hook is reached.
    let number = close_cli_seed_issue("Close me", "New");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease = close_cli_seed_lease_at(&identity, number, "session-a", "active", &worktree);

    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_ne!(exit, 0, "a rejected provider close must fail");
    assert!(
        worktree.exists(),
        "a failed provider close must leave the worktree directory untouched"
    );
    assert_eq!(close_cli_lease_state(&lease).0, "active");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Issue 575 Phase 1: the close convergence is not session-scoped.
//
// `issue close` no longer needs a session identity: every `active` lease of
// `(repo, issue)` flips to `retained` and the guarded removal runs on the
// same command, while `--worktree-session` stays optional and only names the
// closer in the release reason. These tests drive the real CLI from a
// checkout with `PHASEGENT_SESSION_ID` removed, so the legacy fallback (the
// no-session path that used to leave every lease and every directory alone)
// is the one under test.
// ---------------------------------------------------------------------------

/// RAII helper that removes `PHASEGENT_SESSION_ID` for the duration of a
/// test and restores the host value on Drop so the session-less close path
/// is exercised deterministically.
struct SessionEnvRestore(Option<std::ffi::OsString>);

impl SessionEnvRestore {
    fn remove() -> Self {
        let previous = std::env::var_os("PHASEGENT_SESSION_ID");
        // SAFETY: serialised by `lock_workflow_tests`; the Drop guard
        // restores the host value when the test unwinds.
        unsafe {
            std::env::remove_var("PHASEGENT_SESSION_ID");
        }
        Self(previous)
    }
}

impl Drop for SessionEnvRestore {
    fn drop(&mut self) {
        let previous = self.0.take();
        // SAFETY: symmetric with `remove` above.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("PHASEGENT_SESSION_ID", value),
                None => std::env::remove_var("PHASEGENT_SESSION_ID"),
            }
        }
    }
}

#[test]
fn cli_issue_close_without_session_cleans_and_retains_every_lease() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let _session_env = SessionEnvRestore::remove();
    let root = close_cli_root("cleanup-no-session");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let first = close_cli_add_worktree(&repo, "no-session-a", "feat/575-a");
    let second = close_cli_add_worktree(&repo, "no-session-b", "feat/575-b");
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    // Two sessions hold active leases for the issue, each on its own clean
    // directory, and neither is the (absent) closer.
    let lease_a = close_cli_seed_lease_at(&identity, number, "session-a", "active", &first);
    let lease_b = close_cli_seed_lease_at(&identity, number, "session-b", "active", &second);

    let exit = close_cli_run(number, None, &repo);

    assert_eq!(
        exit, 0,
        "the local-provider close must succeed without a session identity"
    );
    assert!(
        !first.exists() && !second.exists(),
        "both clean worktree directories must be removed by a session-less close"
    );
    let (status_a, reason_a) = close_cli_lease_state(&lease_a);
    assert_eq!(status_a, "retained");
    assert_eq!(reason_a.as_deref(), Some("issue closed"));
    let (status_b, reason_b) = close_cli_lease_state(&lease_b);
    assert_eq!(status_b, "retained");
    assert_eq!(reason_b.as_deref(), Some("issue closed"));
    assert!(
        close_cli_branch_exists(&repo, "feat/575-a")
            && close_cli_branch_exists(&repo, "feat/575-b"),
        "the branches must never be deleted"
    );
    let _ = fs::remove_dir_all(&first);
    let _ = fs::remove_dir_all(&second);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_without_session_keeps_dirty_worktree() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let _session_env = SessionEnvRestore::remove();
    let root = close_cli_root("cleanup-no-session-dirty");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "no-session-dirty", "feat/575-dirty");
    // An untracked file is enough to make the directory dirty.
    fs::write(worktree.join("scratch.txt"), "wip").unwrap();
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease = close_cli_seed_lease_at(&identity, number, "session-a", "active", &worktree);

    let exit = close_cli_run(number, None, &repo);

    assert_eq!(exit, 0);
    assert!(
        worktree.exists(),
        "a dirty worktree must be kept even without a session identity"
    );
    assert_eq!(
        close_cli_lease_state(&lease).0,
        "retained",
        "the lease still converges when the directory is kept"
    );
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_without_session_keeps_another_issues_worktree() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let _session_env = SessionEnvRestore::remove();
    let root = close_cli_root("cleanup-no-session-other-issue");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let ours = close_cli_add_worktree(&repo, "no-session-ours", "feat/575-ours");
    let theirs = close_cli_add_worktree(&repo, "no-session-theirs", "feat/9002-theirs");
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    close_cli_seed_lease_at(&identity, number, "session-a", "active", &ours);
    let other = close_cli_seed_lease_at(&identity, 9002, "session-b", "active", &theirs);

    let exit = close_cli_run(number, None, &repo);

    assert_eq!(exit, 0);
    assert!(
        !ours.exists(),
        "the closed issue's clean directory is removed without a session"
    );
    assert!(
        theirs.exists(),
        "another issue's active lease still protects its directory"
    );
    assert_eq!(close_cli_lease_state(&other).0, "active");
    let _ = fs::remove_dir_all(&ours);
    let _ = fs::remove_dir_all(&theirs);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_with_session_names_the_closer_in_the_reason() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("cleanup-session-reason");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let worktree = close_cli_add_worktree(&repo, "session-reason", "feat/575-reason");
    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease = close_cli_seed_lease_at(&identity, number, "session-b", "active", &worktree);

    // `--worktree-session` is optional now: given one, it only attributes
    // the release reason, including for another session's lease row.
    let exit = close_cli_run(number, Some("session-a"), &repo);

    assert_eq!(exit, 0);
    let (status, reason) = close_cli_lease_state(&lease);
    assert_eq!(status, "retained");
    assert_eq!(reason.as_deref(), Some("issue closed: session-a"));
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Issue 552 Phase 2: `issue sync` reconciles an issue the provider already
// closed, and `worktree acquire|list|prune` run the same pass before their
// own work.
//
// The pass itself is driven through `cli::issue::sync::run_sync` so the
// report object is asserted field by field; the CLI wiring is driven through
// `cli::issue::execute_issue` (direct sync) and
// `cli::worktree::execute_worktree` (taxi). Every test pins its database,
// local database, and git checkouts to temp paths.
// ---------------------------------------------------------------------------
