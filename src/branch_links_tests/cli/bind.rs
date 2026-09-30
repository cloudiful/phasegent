//! Scoped `issue bind` flows end to end: isolated temp repo plus temp DB
//! under the workflow lock; the live store is never touched. Shared
//! fixtures live in the parent `cli` module.

use super::{
    BRANCH, TempRepo, db_links, in_temp_repo, local_key, redmine_scope, scoped_bind, scoped_env,
};
use crate::branch_links;
use crate::command::IssueCommand;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use crate::providers::ProviderKind;

#[test]
fn scoped_bind_links_durably() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("scoped-bind");
    repo.checkout_branch(BRANCH);
    let (dir, _db, _index, _env) = scoped_env("scoped-bind");

    assert_eq!(scoped_bind(&repo, 628), 0, "scoped bind must succeed");
    let links = db_links(&dir, &local_key(&repo), BRANCH);
    assert_eq!(links.len(), 1, "exactly one durable link expected");
    assert_eq!(links[0].issue_number, 628);
    assert_eq!(links[0].status, "linked");

    // Repeat bind is the idempotent no-op.
    assert_eq!(
        scoped_bind(&repo, 628),
        0,
        "repeat bind must stay successful"
    );
    let links = db_links(&dir, &local_key(&repo), BRANCH);
    assert_eq!(links.len(), 1, "repeat bind must not duplicate");
}

#[test]
fn scoped_bind_adds_an_ambiguous_second_link() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("scoped-ambiguous");
    repo.checkout_branch(BRANCH);
    let (dir, _db, _index, _env) = scoped_env("scoped-ambiguous");

    assert_eq!(scoped_bind(&repo, 628), 0);
    assert_eq!(scoped_bind(&repo, 616), 0);
    let links = db_links(&dir, &local_key(&repo), BRANCH);
    assert_eq!(links.len(), 2, "distinct links are both kept");
    assert!(
        links.iter().all(|entry| entry.status == "linked"),
        "history is never replaced: {links:?}"
    );
}

#[test]
fn unscoped_bind_fails_closed_with_a_scope_error() {
    // No explicit scope and no stored provider selection: the bind must
    // fail with the structured scope error instead of falling back to
    // Git config or guessing a provider.
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("unscoped-scope");
    repo.checkout_branch(BRANCH);
    let (dir, _db, _index, _env) = scoped_env("unscoped-scope");

    let exit = in_temp_repo(&repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(Role::Orchestrator),
            None,
            None,
            None,
            IssueCommand::Bind { issue_id: 541 },
        )
    });
    assert_eq!(exit, 1, "an unselected provider must fail closed");
    assert!(
        db_links(&dir, &local_key(&repo), BRANCH).is_empty(),
        "a refused bind must not write a link"
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
            IssueCommand::Bind { issue_id: 628 },
        )
    });
    assert_eq!(exit, 1, "unscoped redmine bind must fail closed");
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
            IssueCommand::Bind { issue_id: 700 },
        )
    });
    assert_eq!(exit, 1, "default branch bind must be rejected");
    let key = branch_links::repo_key_for_origin("https://forge.example.com/owner/repo.git")
        .expect("canonical key");
    assert!(
        db_links(&dir, &key, "main").is_empty(),
        "rejected bind must not write durable links"
    );
}
