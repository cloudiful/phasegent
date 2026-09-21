use super::support::*;
use super::*;

fn close_cli_seed_lease(identity: &str, issue: u64, session: &str, status: &str) -> String {
    let storage = Storage::open().unwrap();
    crate::worktree::ensure_schema(&storage).unwrap();
    let lease_id = format!(
        "close-cli-{issue}-{session}-{}",
        crate::worktree::compute_fingerprint(identity)
    );
    let now = crate::worktree::now_unix_secs();
    let worktree_path = format!("/tmp/phasegent-close-cli-{issue}-{session}");
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: &lease_id,
            identity,
            issue,
            session,
            checkout_path: "/tmp/phasegent-close-cli-checkout",
            worktree_path: &worktree_path,
            branch: "main",
            status,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .unwrap();
    lease_id
}

/// RAII helper that removes `PHASEGENT_SESSION_ID` for the duration of a
/// test and restores the host value on Drop so the no-session path is
/// exercised deterministically.
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
fn cli_issue_close_releases_every_session_lease_after_provider_success() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("success");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repo).unwrap();

    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease_a = close_cli_seed_lease(&identity, number, "session-a", "active");
    let lease_b = close_cli_seed_lease(&identity, number, "session-b", "active");

    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
        None,
        None,
        command::IssueCommand::Close {
            number,
            worktree_session: Some("session-a".to_owned()),
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);

    assert_eq!(exit, 0, "a valid local-provider close must succeed");
    let (status_a, reason_a) = close_cli_lease_state(&lease_a);
    assert_eq!(status_a, "retained");
    assert_eq!(reason_a.as_deref(), Some("issue closed: session-a"));
    let (status_b, reason_b) = close_cli_lease_state(&lease_b);
    assert_eq!(
        status_b, "retained",
        "a second session's lease for the closed issue must be converged"
    );
    assert_eq!(reason_b.as_deref(), Some("issue closed: session-a"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_provider_failure_leaves_lease_active() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let root = close_cli_root("failure");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repo).unwrap();

    // `New -> Closed` is rejected by the local transition policy, so the
    // provider close fails before the lease hook is reached.
    let number = close_cli_seed_issue("Close me", "New");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease_a = close_cli_seed_lease(&identity, number, "session-a", "active");

    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
        None,
        None,
        command::IssueCommand::Close {
            number,
            worktree_session: Some("session-a".to_owned()),
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);

    assert_ne!(exit, 0, "a rejected provider close must fail");
    assert_eq!(
        close_cli_lease_state(&lease_a).0,
        "active",
        "a failed provider close must not release the lease"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_issue_close_without_session_leaves_lease_active() {
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    let _lock = lock_workflow_tests();
    let _session_env = SessionEnvRestore::remove();
    let root = close_cli_root("no-session");
    let db = root.join("phasegent.sqlite3");
    let local_db = root.join("phasegent-local.sqlite3");
    let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db.to_string_lossy().as_ref());
    let _local_guard = EnvGuard::set(
        "PHASEGENT_LOCAL_DB_PATH",
        local_db.to_string_lossy().as_ref(),
    );

    let repo = root.join("repo");
    close_cli_init_repo(&repo);
    let previous_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repo).unwrap();

    let number = close_cli_seed_issue("Close me", "Resolved");
    let identity =
        crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo)
            .unwrap();
    let lease_a = close_cli_seed_lease(&identity, number, "session-a", "active");
    let lease_b = close_cli_seed_lease(&identity, number, "session-b", "active");

    let exit = crate::cli::issue::execute_issue(
        Some(Role::Orchestrator),
        Some(ProviderKind::Local),
        None,
        None,
        None,
        None,
        command::IssueCommand::Close {
            number,
            worktree_session: None,
        },
    );
    let _ = std::env::set_current_dir(&previous_cwd);

    // The provider close succeeded (exit 0), so the stdout envelope is the
    // unchanged provider close document; no owner is guessed locally.
    assert_eq!(exit, 0);
    assert_eq!(close_cli_lease_state(&lease_a).0, "active");
    assert_eq!(close_cli_lease_state(&lease_b).0, "active");
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Issue 552 Phase 1: `issue close` removes the closed issue's clean worktree
// directories after the provider close and the lease release.
//
// These tests drive `execute_issue` end-to-end against the deterministic
// local provider with a temp worktree database and a temp git checkout whose
// issue has a real linked worktree. They prove the guard split (clean
// directory removed; dirty directory, another issue's directory, and the main
// checkout kept), the branch preservation, and that a rejected provider close
// leaves the directory untouched.
// ---------------------------------------------------------------------------
