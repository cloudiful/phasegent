//! `issue branches N` reverse-lookup flows (issue 628 P5): isolated temp
//! repo plus temp DB under the workflow lock; the live store is never
//! touched. Shared fixtures live in the parent `cli` module.

use super::{TempRepo, in_temp_repo, local_key, scoped_env};
use crate::branch_links;
use crate::infra::storage::test_support::lock_workflow_tests;
use std::path::Path;

fn seed_scoped_link(
    db: &Path,
    repo_key: &str,
    branch: &str,
    provider: &str,
    project: &str,
    issue: u64,
) {
    let storage = crate::infra::storage::Storage::open_at(&db.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    branch_links::ensure_schema(&storage.connection).expect("schema");
    let key = branch_links::IssueKey::new(provider, project, issue.to_string()).expect("key");
    branch_links::store::link(
        &storage.connection,
        &branch_links::LinkParams {
            repo_key,
            branch,
            issue: &key,
            issue_number: issue,
            source: "test",
            now: 1_700_000_001,
        },
    )
    .expect("link must insert");
}

#[test]
fn branches_lists_every_branch_for_one_number_across_branches() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("branches-multi");
    repo.checkout_branch("feat/628");
    let (dir, _db, _index, _env) = scoped_env("branches-multi");
    let key = local_key(&repo);
    seed_scoped_link(&dir, &key, "feat/628", "redmine", "tools-phasegent", 628);
    seed_scoped_link(&dir, &key, "fix/628", "redmine", "tools-phasegent", 628);

    let storage = crate::infra::storage::Storage::open_at(&dir.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    let rows = crate::branch_links::reads::branches_for_number(
        &storage.connection,
        &key,
        628,
        true,
        &branch_links::UnknownState,
    )
    .expect("read must work");
    assert_eq!(rows.len(), 2, "both branches stay linked: {rows:?}");
    let names: Vec<&str> = rows.iter().map(|entry| entry.branch.as_str()).collect();
    assert!(names.contains(&"feat/628") && names.contains(&"fix/628"));
    assert!(
        rows.iter()
            .all(|entry| entry.issue_number == 628 && entry.status == "linked"),
        "rows untouched: {rows:?}"
    );
}

#[test]
fn branches_keeps_cross_scope_same_number_distinct() {
    // Two distinct provider/project identities sharing one number stay
    // distinct rows (issue 628): the reverse lookup never collapses
    // them into one scope.
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("branches-cross-scope");
    repo.checkout_branch("feat/628");
    let (dir, _db, _index, _env) = scoped_env("branches-cross-scope");
    let key = local_key(&repo);
    seed_scoped_link(&dir, &key, "feat/628", "redmine", "tools-phasegent", 628);
    seed_scoped_link(&dir, &key, "feat/628-x", "forgejo", "acme/widgets", 628);

    let storage = crate::infra::storage::Storage::open_at(&dir.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    let rows = crate::branch_links::reads::branches_for_number(
        &storage.connection,
        &key,
        628,
        true,
        &branch_links::UnknownState,
    )
    .expect("read must work");
    assert_eq!(rows.len(), 2, "both scopes stay listed: {rows:?}");
    let scopes: std::collections::BTreeSet<(&str, &str)> = rows
        .iter()
        .map(|entry| (entry.issue.provider.as_str(), entry.issue.project.as_str()))
        .collect();
    assert_eq!(scopes.len(), 2, "scopes stay distinct: {scopes:?}");
}

#[test]
fn branches_empty_when_no_links() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("branches-empty");
    repo.checkout_branch("feat/628");
    let (dir, _db, _index, _env) = scoped_env("branches-empty");
    let key = local_key(&repo);

    let storage = crate::infra::storage::Storage::open_at(&dir.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    branch_links::ensure_schema(&storage.connection).expect("schema");
    let rows = crate::branch_links::reads::branches_for_number(
        &storage.connection,
        &key,
        628,
        true,
        &branch_links::UnknownState,
    )
    .expect("read must work");
    assert!(rows.is_empty(), "no links yields no rows");
}

#[test]
fn branches_cli_succeeds_without_touching_status_compat() {
    // The new reverse lookup is additive: current-branch `issue status`
    // keeps its compatible shape and exit code alongside it.
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("branches-cli");
    repo.checkout_branch("feat/628");
    let (dir, _db, _index, _env) = scoped_env("branches-cli");
    let key = local_key(&repo);
    seed_scoped_link(&dir, &key, "feat/628", "redmine", "tools-phasegent", 628);

    let branches_exit = in_temp_repo(&repo, || crate::cli::issue_branches::execute_branches(628));
    assert_eq!(branches_exit, 0, "branches must succeed");
    let status_exit = in_temp_repo(&repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(crate::policy::Role::Orchestrator),
            None,
            None,
            None,
            crate::command::IssueCommand::StatusBranch,
        )
    });
    assert_eq!(status_exit, 0, "status compat stays green");
}
