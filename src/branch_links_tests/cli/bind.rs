//! Scoped `issue bind` flows end to end (issue 628 P3): isolated temp
//! repo plus temp DB under the workflow lock; the live store is never
//! touched. Shared fixtures live in the parent `cli` module.

use super::{
    BRANCH, TempRepo, db_links, in_temp_repo, local_key, redmine_scope, scoped_bind, scoped_env,
    scoped_status,
};
use crate::branch_links;
use crate::command::IssueCommand;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use crate::providers::ProviderKind;

#[test]
fn scoped_bind_links_without_touching_git_keys() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("scoped-bind");
    repo.checkout_branch(BRANCH);
    let (dir, _db, _index, _env) = scoped_env("scoped-bind");

    assert_eq!(scoped_bind(&repo, 628), 0, "scoped bind must succeed");
    // No Git key written: durable links only.
    assert_eq!(repo.get_binding(BRANCH), None);
    let links = db_links(&dir, &local_key(&repo), BRANCH);
    assert_eq!(links.len(), 1, "exactly one durable link expected");
    assert_eq!(links[0].issue_number, 628);
    assert_eq!(links[0].status, "linked");

    // Repeat bind is the idempotent no-op (hook-safe, like legacy).
    assert_eq!(
        scoped_bind(&repo, 628),
        0,
        "repeat bind must stay successful"
    );
    let links = db_links(&dir, &local_key(&repo), BRANCH);
    assert_eq!(links.len(), 1, "repeat bind must not duplicate");
}

#[test]
fn scoped_bind_imports_legacy_keys_and_status_shows_both() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("scoped-import");
    repo.checkout_branch(BRANCH);
    repo.set_binding(BRANCH, 628);
    let (dir, _db, _index, _env) = scoped_env("scoped-import");

    // Binding another issue migrates the legacy key first, then adds.
    assert_eq!(
        scoped_bind(&repo, 616),
        0,
        "bind with legacy present must succeed"
    );
    // Source Git key untouched by the migration.
    assert_eq!(repo.get_binding(BRANCH).as_deref(), Some("628"));

    let links = db_links(&dir, &local_key(&repo), BRANCH);
    assert_eq!(links.len(), 2, "legacy import plus new link expected");

    // Two distinct links: status stays successful with no guessed issue.
    assert_eq!(
        scoped_status(&repo),
        0,
        "ambiguous status must still succeed"
    );
}

#[test]
fn unscoped_bind_keeps_legacy_git_behavior() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("legacy-fallback");
    repo.checkout_branch(BRANCH);
    let (_dir, _db, _index, _env) = scoped_env("legacy-fallback");

    let exit = in_temp_repo(&repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(Role::Orchestrator),
            None,
            None,
            None,
            IssueCommand::Bind {
                issue_id: 541,
                replace: false,
                session: None,
            },
        )
    });
    assert_eq!(exit, 0, "unscoped bind keeps legacy behavior");
    assert_eq!(
        repo.get_binding(BRANCH).as_deref(),
        Some("541"),
        "legacy path writes the Git key"
    );
}

#[test]
fn blocked_scope_fails_without_writing_anywhere() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("blocked-scope");
    repo.checkout_branch(BRANCH);
    let (dir, _db, _index, _env) = scoped_env("blocked-scope");

    // Redmine selected without a project: BLOCKED, no silent assignment.
    let exit = in_temp_repo(&repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(Role::Orchestrator),
            Some(ProviderKind::Redmine),
            None,
            None,
            IssueCommand::Bind {
                issue_id: 628,
                replace: false,
                session: None,
            },
        )
    });
    assert_eq!(exit, 1, "unscoped redmine bind must fail closed");
    assert_eq!(
        repo.get_binding(BRANCH),
        None,
        "blocked bind must not write Git keys"
    );
    assert!(
        db_links(&dir, &local_key(&repo), BRANCH).is_empty(),
        "blocked bind must not write durable links"
    );
}

#[test]
fn default_branch_bind_is_rejected_before_any_write() {
    let _lock = lock_workflow_tests();
    // Parent init lands on `main`; a cached origin/HEAD marks it default.
    let repo = TempRepo::init("default-guard");
    repo.set_origin("https://forge.example.com/owner/repo.git");
    super::super::run_git(
        &repo.dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let (dir, _db, _index, _env) = scoped_env("default-guard");

    let (provider, repository, project) = redmine_scope();
    let exit = in_temp_repo(&repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(Role::Orchestrator),
            provider,
            repository.as_deref(),
            project.as_deref(),
            IssueCommand::Bind {
                issue_id: 700,
                replace: false,
                session: None,
            },
        )
    });
    assert_eq!(exit, 1, "default branch bind must be rejected");
    assert_eq!(repo.get_binding("main"), None);
    let key = branch_links::repo_key_for_origin("https://forge.example.com/owner/repo.git")
        .expect("canonical key");
    assert!(
        db_links(&dir, &key, "main").is_empty(),
        "rejected bind must not write durable links"
    );
}
