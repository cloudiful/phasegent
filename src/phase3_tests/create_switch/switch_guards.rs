//! Detached/blocked/mismatch/existing guards (issue 628 P4).
//!
//! Moved verbatim from the parent module; shared fixtures stay in the
//! parent `create_switch` module.

use super::{
    branch_exists, canonical_key, current_branch, linked_numbers, pin_temp_db, switch_params,
    switch_repo,
};
use crate::git_runner::GitRunner;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::lifecycle;

#[test]
fn create_switch_detached_head_warns_without_writing() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("detached") else {
        return;
    };
    let _env = pin_temp_db(&db);
    repo.runner()
        .run(&["checkout", "--detach", "HEAD"])
        .expect("detach works");
    let wt = crate::worktree::ProcessWorktreeRunner::new();

    let outcome = lifecycle::create_link_and_switch(
        &repo.runner(),
        &wt,
        &switch_params(&repo, 628, "feat/628", Some("tools-phasegent")),
    );
    let lifecycle::CreateSwitchOutcome::Warning { reason } = outcome else {
        panic!("detached HEAD must warn: {outcome:?}");
    };
    assert!(reason.contains("detached"), "{reason}");
    assert!(
        !branch_exists(&repo, "feat/628"),
        "detached HEAD must not create the branch"
    );
    assert!(
        linked_numbers(&db, &canonical_key(), "feat/628").is_empty(),
        "detached HEAD must not link"
    );
}

#[test]
fn create_switch_blocked_without_project_writes_nothing() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("blocked") else {
        return;
    };
    let _env = pin_temp_db(&db);
    let wt = crate::worktree::ProcessWorktreeRunner::new();

    let outcome = lifecycle::create_link_and_switch(
        &repo.runner(),
        &wt,
        &switch_params(&repo, 628, "feat/628", None),
    );
    let lifecycle::CreateSwitchOutcome::Blocked { reason } = outcome else {
        panic!("missing project must block: {outcome:?}");
    };
    assert!(reason.contains("project"), "{reason}");
    assert!(!branch_exists(&repo, "feat/628"), "blocked must not create");
    assert!(
        linked_numbers(&db, &canonical_key(), "feat/628").is_empty(),
        "blocked must not link"
    );
}

#[test]
fn create_switch_skips_silently_on_repository_mismatch() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("mismatch") else {
        return;
    };
    // No DB pin needed: the mismatch gate fires before any store IO.
    // Drop the unused scratch dir the helper prepared.
    let _ = std::fs::remove_dir_all(&db);
    let wt = crate::worktree::ProcessWorktreeRunner::new();
    let mut params = switch_params(&repo, 628, "feat/628", Some("tools-phasegent"));
    params.explicit_repository = Some("other/tools");
    let outcome = lifecycle::create_link_and_switch(&repo.runner(), &wt, &params);
    assert!(
        matches!(outcome, lifecycle::CreateSwitchOutcome::Skipped { .. }),
        "foreign checkout must skip: {outcome:?}"
    );
    assert!(outcome.warning().is_none());
    assert!(!branch_exists(&repo, "feat/628"));
}

#[test]
fn create_switch_links_existing_branch_and_moves() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("existing") else {
        return;
    };
    let _env = pin_temp_db(&db);
    repo.runner()
        .run(&["branch", "feat/628", "HEAD"])
        .expect("pre-create works");
    let wt = crate::worktree::ProcessWorktreeRunner::new();

    let outcome = lifecycle::create_link_and_switch(
        &repo.runner(),
        &wt,
        &switch_params(&repo, 628, "feat/628", Some("tools-phasegent")),
    );
    assert_eq!(
        outcome,
        lifecycle::CreateSwitchOutcome::Switched {
            branch: "feat/628".to_owned(),
            issue_id: 628,
        }
    );
    assert_eq!(current_branch(&repo), "feat/628");
    assert_eq!(linked_numbers(&db, &canonical_key(), "feat/628"), vec![628]);
}
