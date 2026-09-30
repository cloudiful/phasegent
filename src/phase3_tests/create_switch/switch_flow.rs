//! Clean/dirty/unknown/default-gate switching (issue 628 P4).
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
fn create_switch_moves_clean_default_checkout() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("clean") else {
        return;
    };
    let _env = pin_temp_db(&db);
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
    assert!(outcome.warning().is_none());
    assert_eq!(current_branch(&repo), "feat/628");
    assert_eq!(
        linked_numbers(&db, &canonical_key(), "feat/628"),
        vec![628],
        "the issue branch must be linked"
    );
}

#[test]
fn create_switch_leaves_dirty_checkout_with_link() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("dirty") else {
        return;
    };
    let _env = pin_temp_db(&db);
    std::fs::write(repo.0.join("scratch.txt"), "scratch\n").expect("dirty the tree");
    let wt = crate::worktree::ProcessWorktreeRunner::new();

    let outcome = lifecycle::create_link_and_switch(
        &repo.runner(),
        &wt,
        &switch_params(&repo, 628, "feat/628", Some("tools-phasegent")),
    );
    let lifecycle::CreateSwitchOutcome::LinkedOnly {
        branch,
        issue_id,
        reason,
    } = outcome
    else {
        panic!("dirty checkout must link without switching: {outcome:?}");
    };
    assert_eq!(branch, "feat/628");
    assert_eq!(issue_id, 628);
    assert!(
        reason.contains("dirty") && reason.contains("git switch feat/628"),
        "warning must name the cause and the manual action: {reason}"
    );
    assert!(
        branch_exists(&repo, "feat/628"),
        "the branch is still created"
    );
    assert_eq!(
        linked_numbers(&db, &canonical_key(), "feat/628"),
        vec![628],
        "the link is still recorded"
    );
    assert_eq!(current_branch(&repo), "main", "checkout stays untouched");
    let _ = std::fs::remove_file(repo.0.join("scratch.txt"));
}

#[test]
fn create_switch_unknown_probe_leaves_checkout() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("unknown") else {
        return;
    };
    let _env = pin_temp_db(&db);
    // Probe a directory that is not a checkout: cleanliness is
    // unknown, so the switch must not happen.
    let elsewhere = crate::test_scratch::root().join(format!(
        "phasegent-phase3-elsewhere-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&elsewhere).expect("scratch dir must create");
    let wt = crate::worktree::ProcessWorktreeRunner::new();
    let mut params = switch_params(&repo, 628, "feat/628", Some("tools-phasegent"));
    params.repo_path = &elsewhere;
    let outcome = lifecycle::create_link_and_switch(&repo.runner(), &wt, &params);
    assert!(
        matches!(outcome, lifecycle::CreateSwitchOutcome::LinkedOnly { .. }),
        "unknown cleanliness must link without switching: {outcome:?}"
    );
    assert!(
        outcome.warning().is_some_and(|w| w.contains("unknown")),
        "warning must say unknown: {:?}",
        outcome.warning()
    );
    assert_eq!(current_branch(&repo), "main");
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn create_switch_refuses_non_default_current_branch() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("nondefault") else {
        return;
    };
    let _env = pin_temp_db(&db);
    repo.runner()
        .run(&["checkout", "-q", "-b", "feat/other"])
        .expect("checkout feature works");
    let wt = crate::worktree::ProcessWorktreeRunner::new();

    let outcome = lifecycle::create_link_and_switch(
        &repo.runner(),
        &wt,
        &switch_params(&repo, 628, "feat/628", Some("tools-phasegent")),
    );
    let lifecycle::CreateSwitchOutcome::LinkedOnly { reason, .. } = outcome else {
        panic!("non-default current must link without switching: {outcome:?}");
    };
    assert!(
        reason.contains("feat/other") && reason.contains("main"),
        "warning must name current and default: {reason}"
    );
    assert_eq!(current_branch(&repo), "feat/other");
    assert_eq!(linked_numbers(&db, &canonical_key(), "feat/628"), vec![628]);
}

#[test]
fn create_switch_unknown_default_leaves_checkout() {
    let _lock = lock_workflow_tests();
    let Some(repo) = super::super::TempRepo::new("nodefault") else {
        return;
    };
    // No origin at all: no cached default can be detected.
    let git = repo.runner();
    git.run(&[
        "-c",
        "user.name=phasegent-test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--allow-empty",
        "-q",
        "-m",
        "init",
    ])
    .expect("commit works");
    git.run(&["checkout", "-q", "-B", "main"])
        .expect("checkout works");
    let db = crate::test_scratch::root().join(format!(
        "phasegent-phase3-nodefault-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&db).expect("scratch dir must create");
    let _env = pin_temp_db(&db);
    let wt = crate::worktree::ProcessWorktreeRunner::new();

    let outcome = lifecycle::create_link_and_switch(
        &git,
        &wt,
        &switch_params(&repo, 628, "feat/628", Some("tools-phasegent")),
    );
    assert!(
        matches!(outcome, lifecycle::CreateSwitchOutcome::LinkedOnly { .. }),
        "unknown default must link without switching: {outcome:?}"
    );
    assert_eq!(current_branch(&repo), "main");
    // Local-only link: no origin means a `local:` key.
    let toplevel = git
        .run(&["rev-parse", "--show-toplevel"])
        .expect("toplevel resolves");
    assert_eq!(toplevel.status, 0);
    let local =
        crate::branch_links::resolve_repo_key(None, std::path::Path::new(toplevel.stdout.trim()))
            .expect("fallback key");
    assert!(local.local_only);
    assert_eq!(linked_numbers(&db, &local.key, "feat/628"), vec![628]);
}
