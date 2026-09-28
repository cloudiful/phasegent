//! Conventional-default protection and hook behavior (issue 628).
//!
//! Moved verbatim from the parent `issue_switch` module so protection
//! coverage lives on its own. Shared fixtures stay in the parent.

use super::super::pin_temp_db;
use super::{bare_repo, fallback_key, seed_link};
use crate::branch_context::GitRunner;
use crate::branch_links::identity::{is_protected_branch, validate_not_protected_branch};
use crate::infra::storage::test_support::lock_workflow_tests;

#[test]
fn protected_branch_rule_table() {
    assert!(is_protected_branch("main", None));
    assert!(is_protected_branch("master", None));
    assert!(!is_protected_branch("feat/1", None));
    assert!(!is_protected_branch("develop", None));
    assert!(!is_protected_branch("", None));
    assert!(is_protected_branch("main", Some("main")));
    assert!(!is_protected_branch("feat/1", Some("main")));
    // A known non-conventional default protects only itself.
    assert!(!is_protected_branch("main", Some("develop")));
    assert!(is_protected_branch("develop", Some("develop")));
    // Blank detection output counts as unknown.
    assert!(is_protected_branch("main", Some("  ")));

    let detected = validate_not_protected_branch("main", Some("main")).expect_err("refused");
    assert!(detected.contains("detected default"), "{detected}");
    let conventional = validate_not_protected_branch("master", None).expect_err("refused");
    assert!(conventional.contains("conventional"), "{conventional}");
    assert!(validate_not_protected_branch("feat/1", None).is_ok());
    assert!(validate_not_protected_branch("feat/1", Some("main")).is_ok());
}

#[test]
fn hook_ignores_durable_links_on_conventional_default_when_unknown() {
    // `main` with no cached HEAD: durable rows do not decide (the
    // explicit legacy binding below still does).
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = bare_repo("hook-conventional") else {
        return;
    };
    let _env = pin_temp_db(&db);
    repo.runner()
        .run(&["checkout", "-q", "main"])
        .expect("checkout works");
    seed_link(
        &db,
        &fallback_key(&repo),
        "main",
        "redmine",
        "tools-phasegent",
        700,
    );

    let file = repo.0.join("COMMIT_MSG");
    std::fs::write(&file, "Work on main\n").expect("write message");
    let value = crate::hooks::run_with(
        &repo.runner(),
        crate::hooks::HookKind::PrepareCommitMsg,
        file.to_str().expect("utf8 path"),
        Some(""),
    )
    .expect("hook runs");
    assert_eq!(value["action"], "noop", "durable links on main stay silent");
    assert_eq!(
        std::fs::read(&file).expect("read message"),
        b"Work on main\n"
    );
}

#[test]
fn hook_keeps_legacy_binding_on_conventional_default_when_unknown() {
    // Same checkout, but the explicit legacy binding (not a durable
    // guess) still decides.
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = bare_repo("hook-legacy-main") else {
        return;
    };
    let _env = pin_temp_db(&db);
    repo.runner()
        .run(&["checkout", "-q", "main"])
        .expect("checkout works");
    repo.runner()
        .run(&["config", "--local", "branch.main.redmine-issue-id", "701"])
        .expect("legacy bind works");

    let file = repo.0.join("COMMIT_MSG");
    std::fs::write(&file, "Legacy-bound work\n").expect("write message");
    let value = crate::hooks::run_with(
        &repo.runner(),
        crate::hooks::HookKind::PrepareCommitMsg,
        file.to_str().expect("utf8 path"),
        Some(""),
    )
    .expect("hook runs");
    assert_eq!(value["action"], "appended");
    assert_eq!(
        std::fs::read(&file).expect("read message"),
        b"Legacy-bound work\n\nRefs #701\n"
    );
}

#[test]
fn hook_still_resolves_normal_branches_when_default_unknown() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = bare_repo("hook-normal-unknown") else {
        return;
    };
    let _env = pin_temp_db(&db);
    repo.runner()
        .run(&["checkout", "-q", "-b", "feat/9"])
        .expect("checkout works");
    seed_link(
        &db,
        &fallback_key(&repo),
        "feat/9",
        "redmine",
        "tools-phasegent",
        9,
    );

    let file = repo.0.join("COMMIT_MSG");
    std::fs::write(&file, "Feature work\n").expect("write message");
    let value = crate::hooks::run_with(
        &repo.runner(),
        crate::hooks::HookKind::PrepareCommitMsg,
        file.to_str().expect("utf8 path"),
        Some(""),
    )
    .expect("hook runs");
    assert_eq!(value["action"], "appended");
    assert_eq!(
        std::fs::read(&file).expect("read message"),
        b"Feature work\n\nRefs #9\n"
    );
}
