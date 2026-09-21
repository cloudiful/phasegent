use super::support::*;
use super::*;

#[test]
fn close_cleanup_removes_clean_worktree_and_keeps_branch() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("cleanup-clean");
    let Some((repo_dir, identity)) = temp_git_repo("cleanup-clean") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let worktree = add_cleanup_worktree(&repo_dir, "clean", "feat/552-clean");
    let lease = seed_lease_at(&identity, 552, "session-a", "retained", &worktree);

    let outcome = cleanup_outcome(&repo_dir, 552, Some("session-a"));
    assert_eq!(
        outcome,
        crate::lifecycle::AutoCleanupOutcome::Cleaned {
            removed: 1,
            kept: Vec::new(),
        }
    );
    assert!(
        outcome.warnings().is_empty(),
        "a fully successful cleanup stays silent on stderr"
    );
    assert!(
        !worktree.exists(),
        "the clean retained worktree directory must be removed"
    );
    assert_eq!(
        lease_state(&lease).0,
        "retained",
        "the cleanup must only read lease rows"
    );
    assert!(
        branch_exists(&repo_dir, "feat/552-clean"),
        "the branch must never be deleted"
    );
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_cleanup_keeps_dirty_worktree_with_reason() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("cleanup-dirty");
    let Some((repo_dir, identity)) = temp_git_repo("cleanup-dirty") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let worktree = add_cleanup_worktree(&repo_dir, "dirty", "feat/553-dirty");
    fs::write(worktree.join("scratch.txt"), "wip").expect("write untracked file");
    seed_lease_at(&identity, 553, "session-a", "retained", &worktree);

    let outcome = cleanup_outcome(&repo_dir, 553, Some("session-a"));
    match &outcome {
        crate::lifecycle::AutoCleanupOutcome::Cleaned { removed, kept } => {
            assert_eq!(*removed, 0, "a dirty directory must not be removed");
            assert_eq!(kept.len(), 1, "the kept directory needs its reason");
            assert!(
                kept[0].contains("uncommitted or untracked files"),
                "the reason must name the dirt: {}",
                kept[0]
            );
            assert!(
                kept[0].contains(worktree.to_str().unwrap()),
                "the reason must name the directory: {}",
                kept[0]
            );
        }
        other => panic!("expected Cleaned, got {other:?}"),
    }
    assert_eq!(outcome.warnings().len(), 1);
    assert!(worktree.exists(), "a dirty worktree directory must be kept");
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_cleanup_keeps_another_sessions_active_lease_directory() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("cleanup-foreign");
    let Some((repo_dir, identity)) = temp_git_repo("cleanup-foreign") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let ours = add_cleanup_worktree(&repo_dir, "ours", "feat/554-ours");
    let theirs = add_cleanup_worktree(&repo_dir, "theirs", "feat/554-theirs");
    seed_lease_at(&identity, 554, "session-a", "retained", &ours);
    let foreign = seed_lease_at(&identity, 554, "session-b", "active", &theirs);

    let outcome = cleanup_outcome(&repo_dir, 554, Some("session-a"));
    match &outcome {
        crate::lifecycle::AutoCleanupOutcome::Cleaned { removed, kept } => {
            assert_eq!(
                *removed, 1,
                "the closer's own retained directory is removed"
            );
            assert_eq!(kept.len(), 1);
            assert!(
                kept[0].contains("session-b") && kept[0].contains("active lease"),
                "the reason must name the foreign active lease: {}",
                kept[0]
            );
        }
        other => panic!("expected Cleaned, got {other:?}"),
    }
    assert!(!ours.exists());
    assert!(
        theirs.exists(),
        "another session's active lease keeps its directory"
    );
    assert_eq!(lease_state(&foreign).0, "active");
    let _ = fs::remove_dir_all(&ours);
    let _ = fs::remove_dir_all(&theirs);
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_cleanup_without_a_session_keeps_active_lease_directories() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("cleanup-no-session");
    let Some((repo_dir, identity)) = temp_git_repo("cleanup-no-session") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let worktree = add_cleanup_worktree(&repo_dir, "no-session", "feat/555-nosession");
    seed_lease_at(&identity, 555, "session-a", "active", &worktree);

    // The legacy fallback has no closer: every active lease is foreign,
    // so an unattributed close keeps the directory.
    let outcome = cleanup_outcome(&repo_dir, 555, None);
    match &outcome {
        crate::lifecycle::AutoCleanupOutcome::Cleaned { removed, kept } => {
            assert_eq!(*removed, 0);
            assert!(kept[0].contains("session-a"), "reason: {}", kept[0]);
        }
        other => panic!("expected Cleaned, got {other:?}"),
    }
    assert!(worktree.exists());
    let _ = fs::remove_dir_all(&worktree);
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_cleanup_never_removes_the_main_checkout() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("cleanup-main");
    let Some((repo_dir, identity)) = temp_git_repo("cleanup-main") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let lease = seed_lease_at(&identity, 556, "session-a", "retained", &repo_dir);

    let outcome = cleanup_outcome(&repo_dir, 556, Some("session-a"));
    match &outcome {
        crate::lifecycle::AutoCleanupOutcome::Cleaned { removed, kept } => {
            assert_eq!(*removed, 0, "the main checkout is never removed");
            assert!(
                kept[0].contains("main checkout"),
                "the reason must name the main checkout: {}",
                kept[0]
            );
        }
        other => panic!("expected Cleaned, got {other:?}"),
    }
    assert!(repo_dir.exists());
    assert_eq!(lease_state(&lease).0, "retained");
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_cleanup_skips_missing_directories_without_warnings() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("cleanup-missing");
    let Some((repo_dir, identity)) = temp_git_repo("cleanup-missing") else {
        let _ = fs::remove_dir_all(temp);
        return;
    };
    let missing = unique_temp_dir("cleanup-missing-dir");
    seed_lease_at(&identity, 557, "session-a", "retained", &missing);

    let outcome = cleanup_outcome(&repo_dir, 557, Some("session-a"));
    assert_eq!(outcome, crate::lifecycle::AutoCleanupOutcome::Noop);
    assert!(outcome.warnings().is_empty());
    let _ = fs::remove_dir_all(repo_dir);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn close_cleanup_warns_when_repo_identity_is_unavailable() {
    let _lock = lock_workflow_tests();
    let (temp, _storage, _env) = open_temp_storage("cleanup-bad-repo");
    // A plain directory that is not a git checkout: the identity cannot
    // be resolved, so nothing may be deleted and the failure degrades to
    // a bounded warning.
    let dir = unique_temp_dir("cleanup-bad-repo-dir");
    fs::create_dir_all(&dir).unwrap();

    let outcome = cleanup_outcome(&dir, 558, Some("session-a"));
    match &outcome {
        crate::lifecycle::AutoCleanupOutcome::Warning { reason } => {
            assert!(
                reason.contains("repository identity"),
                "warning must explain the identity failure: {reason}"
            );
        }
        other => panic!("expected Warning, got {other:?}"),
    }
    assert_eq!(outcome.warnings().len(), 1);
    let _ = fs::remove_dir_all(dir);
    let _ = fs::remove_dir_all(temp);
}

// ---------------------------------------------------------------------------
// Issue 443 Phase 2: tool-signal auto-route table.
//
// Pins the `auto_route_next` mapping from the issue examples so the
// timer hooks in `cli/issue.rs` (create -> In Progress) and
// `cli/comment.rs` (create -> In Review) stay stable. The timer role
// for each mapped target must match `status_to_agent_role` without a
// fallback warning, except the close target which intentionally
// falls back (Closed has no canonical role) and is finished via
// `auto_close_issue_timer` rather than a new segment.
// ---------------------------------------------------------------------------
