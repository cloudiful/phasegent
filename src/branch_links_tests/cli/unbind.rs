//! Scoped `issue unbind` flows end to end: isolated temp repo plus temp DB
//! under the workflow lock; the live store is never touched. Shared
//! fixtures live in the parent `cli` module.

use super::{BRANCH, TempRepo, db_links, local_key, scoped_bind, scoped_env, scoped_unbind};
use crate::infra::storage::test_support::lock_workflow_tests;

#[test]
fn scoped_unbind_detaches_with_history() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("scoped-unbind");
    repo.checkout_branch(BRANCH);
    let (dir, _db, _index, _env) = scoped_env("scoped-unbind");

    assert_eq!(scoped_bind(&repo, 628), 0);
    assert_eq!(scoped_bind(&repo, 616), 0);
    assert_eq!(scoped_unbind(&repo), 0, "unbind must succeed");
    let links = db_links(&dir, &local_key(&repo), BRANCH);
    assert_eq!(links.len(), 2, "detach history must be retained");
    assert!(
        links.iter().all(|entry| entry.status == "detached"),
        "every link must be detached: {links:?}"
    );

    assert_eq!(
        scoped_unbind(&repo),
        0,
        "repeat unbind must stay successful"
    );
}

#[test]
fn scoped_unbind_without_links_is_a_noop() {
    let _lock = lock_workflow_tests();
    let repo = TempRepo::init("scoped-unbind-empty");
    repo.checkout_branch(BRANCH);
    let (dir, _db, _index, _env) = scoped_env("scoped-unbind-empty");

    assert_eq!(scoped_unbind(&repo), 0, "empty unbind must stay successful");
    assert!(
        db_links(&dir, &local_key(&repo), BRANCH).is_empty(),
        "nothing to detach and nothing to write"
    );
}
