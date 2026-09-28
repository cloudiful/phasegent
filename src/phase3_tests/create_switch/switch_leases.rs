//! Foreign/own active-lease switching (issue 628 P4).
//!
//! Moved verbatim from the parent module; shared fixtures stay in the
//! parent `create_switch` module.

use super::{current_branch, pin_temp_db, switch_params, switch_repo};
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::lifecycle;

#[test]
fn create_switch_foreign_active_lease_blocks() {
    use crate::worktree::leases::{NewLease, insert_lease};
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("foreign-lease") else {
        return;
    };
    let _env = pin_temp_db(&db);
    let wt = crate::worktree::ProcessWorktreeRunner::new();
    let identity =
        crate::worktree::repo_identity(&wt, &repo.0).expect("lease identity must resolve");
    let storage = crate::infra::storage::Storage::open().expect("temp storage");
    let now = crate::worktree::now_unix_secs();
    let here = repo.0.to_string_lossy().into_owned();
    insert_lease(
        &storage,
        NewLease {
            lease_id: "lease-foreign",
            identity: &identity,
            issue: 600,
            session: "other-session",
            checkout_path: &here,
            worktree_path: &here,
            branch: "feat/600",
            status: crate::worktree::LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("foreign lease");
    drop(storage);

    let outcome = lifecycle::create_link_and_switch(
        &repo.runner(),
        &wt,
        &switch_params(&repo, 628, "feat/628", Some("tools-phasegent")),
    );
    let lifecycle::CreateSwitchOutcome::LinkedOnly { reason, .. } = outcome else {
        panic!("foreign lease must link without switching: {outcome:?}");
    };
    assert!(
        reason.contains("other-session") && reason.contains("--isolate"),
        "warning must name the holder and the escape hatch: {reason}"
    );
    assert_eq!(current_branch(&repo), "main");
}

#[test]
fn create_switch_own_lease_reenters() {
    use crate::worktree::leases::{NewLease, insert_lease};
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("own-lease") else {
        return;
    };
    let _env = pin_temp_db(&db);
    let wt = crate::worktree::ProcessWorktreeRunner::new();
    let identity =
        crate::worktree::repo_identity(&wt, &repo.0).expect("lease identity must resolve");
    let storage = crate::infra::storage::Storage::open().expect("temp storage");
    let now = crate::worktree::now_unix_secs();
    let here = repo.0.to_string_lossy().into_owned();
    insert_lease(
        &storage,
        NewLease {
            lease_id: "lease-own",
            identity: &identity,
            issue: 628,
            session: "session-A",
            checkout_path: &here,
            worktree_path: &here,
            branch: "main",
            status: crate::worktree::LEASE_STATUS_ACTIVE,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("own lease");
    drop(storage);

    let mut params = switch_params(&repo, 628, "feat/628", Some("tools-phasegent"));
    params.session = Some("session-A");
    let outcome = lifecycle::create_link_and_switch(&repo.runner(), &wt, &params);
    assert_eq!(
        outcome,
        lifecycle::CreateSwitchOutcome::Switched {
            branch: "feat/628".to_owned(),
            issue_id: 628,
        },
        "our own triple re-enters"
    );
    assert_eq!(current_branch(&repo), "feat/628");
}
