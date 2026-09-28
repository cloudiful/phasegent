//! Repo-wide active-lease gating for automatic switching (issue 628).
//!
//! Moved verbatim from the parent `issue_switch` module so switch-gate
//! coverage lives on its own. Shared fixtures stay in the parent.

use super::super::{current_branch, pin_temp_db, switch_repo};
use super::{branch_exists, insert_active_lease, repo_lease_identity, switch_params_for};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::lifecycle;

#[test]
fn switch_refuses_when_other_session_holds_lease_elsewhere() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("repo-wide") else {
        return;
    };
    let _env = pin_temp_db(&db);
    // A live lease for another issue on another path in the same repo:
    // the primary checkout must not switch under it.
    let storage = crate::infra::storage::Storage::open().expect("temp storage");
    let identity = repo_lease_identity(&repo);
    let elsewhere = crate::test_scratch::root().join(format!(
        "phasegent-phase3-elsewhere-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&elsewhere).expect("scratch dir must create");
    let other = elsewhere.to_string_lossy().into_owned();
    insert_active_lease(
        &storage,
        &identity,
        "lease-elsewhere",
        600,
        "other-session",
        &other,
        crate::worktree::LEASE_STATUS_ACTIVE,
    );
    drop(storage);
    let wt = crate::worktree::ProcessWorktreeRunner::new();
    let params = switch_params_for(&repo, 628, "feat/628", Some("tools-phasegent"));

    let outcome = lifecycle::create_link_and_switch(&repo.runner(), &wt, &params);
    let lifecycle::CreateSwitchOutcome::LinkedOnly { reason, .. } = outcome else {
        panic!("repo-wide foreign lease must link without switching: {outcome:?}");
    };
    assert!(
        reason.contains("other-session")
            && reason.contains("600")
            && reason.contains("in this repository")
            && reason.contains(other.as_str()),
        "warning must name the holder, issue, path, and repo scope: {reason}"
    );
    assert!(
        branch_exists(&repo, "feat/628"),
        "the branch is still created"
    );
    assert_eq!(current_branch(&repo), "main", "checkout stays untouched");
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn switch_ignores_terminal_leases_elsewhere() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("repo-wide-terminal") else {
        return;
    };
    let _env = pin_temp_db(&db);
    let storage = crate::infra::storage::Storage::open().expect("temp storage");
    let identity = repo_lease_identity(&repo);
    insert_active_lease(
        &storage,
        &identity,
        "lease-old",
        600,
        "other-session",
        "/tmp/phasegent-elsewhere",
        crate::worktree::LEASE_STATUS_RETAINED,
    );
    drop(storage);
    let wt = crate::worktree::ProcessWorktreeRunner::new();
    let params = switch_params_for(&repo, 628, "feat/628", Some("tools-phasegent"));

    let outcome = lifecycle::create_link_and_switch(&repo.runner(), &wt, &params);
    assert_eq!(
        outcome,
        lifecycle::CreateSwitchOutcome::Switched {
            branch: "feat/628".to_owned(),
            issue_id: 628,
        },
        "terminal history elsewhere never blocks"
    );
}
