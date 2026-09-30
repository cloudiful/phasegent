//! Scoped `issue status` flows end to end (issue 628 P3/P4): isolated
//! temp repo plus temp DB under the workflow lock; the live store is
//! never touched. Shared fixtures live in the parent `cli` module.

use super::{BRANCH, TempRepo, db_links, in_temp_repo, local_key, scoped_env, scoped_status};
use crate::branch_links;
use crate::command::IssueCommand;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::policy::Role;
use std::path::Path;

#[test]
fn scoped_status_succeeds_with_one_durable_link() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("scoped-status-linked");
    repo.checkout_branch(BRANCH);
    let (dir, _db, _index, _env) = scoped_env("scoped-status-linked");
    let key = local_key(&repo);
    seed_scoped_link(&dir, &key, BRANCH, "redmine", "tools-phasegent", 628);

    assert_eq!(scoped_status(&repo), 0, "a single durable link resolves");
    let links = db_links(&dir, &key, BRANCH);
    assert_eq!(links.len(), 1, "status is read-only: {links:?}");
    assert_eq!(links[0].status, "linked");
}

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
fn unscoped_status_leaves_cross_scope_same_number_unresolved() {
    // Two distinct provider/project identities sharing one number are
    // never collapsed into a single active issue (issue 628): unscoped
    // status sees both rows and stays successful with no guessed issue.
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("unscoped-cross-scope");
    repo.checkout_branch(BRANCH);
    let (dir, _db, _index, _env) = scoped_env("unscoped-cross-scope");
    let key = local_key(&repo);
    seed_scoped_link(&dir, &key, BRANCH, "redmine", "tools-phasegent", 628);
    seed_scoped_link(&dir, &key, BRANCH, "forgejo", "acme/widgets", 628);

    let exit = in_temp_repo(&repo, || {
        crate::cli::branch::execute_branch_context_scoped(
            Some(Role::Orchestrator),
            None,
            None,
            None,
            IssueCommand::StatusBranch,
        )
    });
    assert_eq!(exit, 0, "ambiguous status must still succeed");
    // The durable rows decide, and they disagree.
    let links = db_links(&dir, &key, BRANCH);
    assert_eq!(links.len(), 2, "both scoped rows stay linked");
    assert!(
        links
            .iter()
            .all(|entry| entry.status == "linked" && entry.issue_number == 628),
        "rows untouched: {links:?}"
    );
}
