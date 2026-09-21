use super::support::*;
use super::*;

#[test]
fn close_release_flips_every_session_lease_for_issue_and_repo() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-release");
    let Some((repo_dir, identity)) = temp_git_repo("current") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let lease_a = seed_lease(&identity, 305, "session-a", "active");
    let lease_b = seed_lease(&identity, 305, "session-b", "active");
    let same_session_other_issue = seed_lease(&identity, 306, "session-a", "active");
    let other_repo = seed_lease("git-common-dir:/elsewhere/.git", 305, "session-a", "active");

    let outcome = crate::lifecycle::release_closed_issue_leases(
        &crate::worktree::ProcessWorktreeRunner::new(),
        &repo_dir,
        305,
        Some("session-a"),
    );
    assert_eq!(
        outcome,
        crate::lifecycle::AutoReleaseLeaseOutcome::Released { released: 2 }
    );
    assert!(outcome.warning().is_none());

    let (status_a, reason_a) = lease_state(&lease_a);
    assert_eq!(status_a, "retained");
    assert_eq!(reason_a.as_deref(), Some("issue closed: session-a"));
    // Issue 537 Phase 2: the second session's active lease for the same
    // issue is converged by the close too, attributed to the closer.
    let (status_b, reason_b) = lease_state(&lease_b);
    assert_eq!(status_b, "retained");
    assert_eq!(reason_b.as_deref(), Some("issue closed: session-a"));
    assert_eq!(lease_state(&same_session_other_issue).0, "active");
    assert_eq!(lease_state(&other_repo).0, "active");
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_release_flips_a_foreign_session_lease_without_a_local_lease() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-foreign-session");
    let Some((repo_dir, identity)) = temp_git_repo("foreign-session") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    // Only a second session holds a lease for the issue: the closer owns
    // nothing, but closing still flips the foreign active lease.
    let foreign = seed_lease(&identity, 538, "session-b", "active");

    let outcome = crate::lifecycle::release_closed_issue_leases(
        &crate::worktree::ProcessWorktreeRunner::new(),
        &repo_dir,
        538,
        Some("session-a"),
    );
    assert_eq!(
        outcome,
        crate::lifecycle::AutoReleaseLeaseOutcome::Released { released: 1 }
    );
    assert!(outcome.warning().is_none());

    let (status, reason) = lease_state(&foreign);
    assert_eq!(status, "retained");
    assert_eq!(reason.as_deref(), Some("issue closed: session-a"));
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_release_without_session_is_noop_with_warning() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-no-session");
    let Some((repo_dir, identity)) = temp_git_repo("no-session") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let lease_a = seed_lease(&identity, 305, "session-a", "active");
    let lease_b = seed_lease(&identity, 305, "session-b", "active");

    let outcome = crate::lifecycle::release_closed_issue_leases(
        &crate::worktree::ProcessWorktreeRunner::new(),
        &repo_dir,
        305,
        None,
    );
    match &outcome {
        crate::lifecycle::AutoReleaseLeaseOutcome::NoSession { reason } => {
            assert!(
                reason.contains("left untouched"),
                "no-session reason must say leases were left untouched: {reason}"
            );
        }
        other => panic!("expected NoSession, got {other:?}"),
    }
    let warning = outcome
        .warning()
        .expect("an absent session must surface a stderr warning");
    assert!(
        warning.contains("PHASEGENT_SESSION_ID"),
        "warning must name the session source, got: {warning}"
    );
    assert_eq!(lease_state(&lease_a).0, "active");
    assert_eq!(lease_state(&lease_b).0, "active");
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_release_with_blank_session_is_noop_with_warning() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-blank-session");
    let Some((repo_dir, identity)) = temp_git_repo("blank-session") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let lease_a = seed_lease(&identity, 305, "session-a", "active");

    let outcome = crate::lifecycle::release_closed_issue_leases(
        &crate::worktree::ProcessWorktreeRunner::new(),
        &repo_dir,
        305,
        Some("   "),
    );
    assert!(matches!(
        outcome,
        crate::lifecycle::AutoReleaseLeaseOutcome::NoSession { .. }
    ));
    assert_eq!(lease_state(&lease_a).0, "active");
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_release_warns_when_repo_identity_is_unavailable() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("close-bad-repo");
    // A plain directory that is not a git checkout: resolving the
    // canonical identity fails, but the remote close already succeeded,
    // so the hook must degrade to a bounded warning instead of failing.
    let dir = unique_temp_dir("close-bad-repo-dir");
    fs::create_dir_all(&dir).unwrap();

    let outcome = crate::lifecycle::release_closed_issue_leases(
        &crate::worktree::ProcessWorktreeRunner::new(),
        &dir,
        305,
        Some("session-a"),
    );
    match &outcome {
        crate::lifecycle::AutoReleaseLeaseOutcome::Warning { reason } => {
            assert!(
                reason.contains("repository identity"),
                "warning must explain the identity failure: {reason}"
            );
        }
        other => panic!("expected Warning, got {other:?}"),
    }
    assert!(outcome.warning().is_some());
    let _ = fs::remove_dir_all(dir);
    let _ = fs::remove_dir_all(temp);
}

// ---------------------------------------------------------------------------
// Issue 552 Phase 1: `issue close` worktree-directory cleanup.
//
// The close chain runs `cleanup_closed_issue_worktrees` right after the
// lease release, so the helper is driven directly here against real
// linked worktrees of a temp repository. These tests pin the three
// guards: a clean directory is removed, a dirty directory stays with its
// reason, another session's active lease protects its directory, the
// main checkout is never removed, and no session supplied keeps every
// active row's directory. Lease rows are only read and branches survive.
// ---------------------------------------------------------------------------
