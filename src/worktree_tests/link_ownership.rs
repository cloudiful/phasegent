//! Durable link issue context for lease decisions (issue 628 P4,
//! hardened in P4 attempt 2): exactly one distinct
//! `(provider, project, issue_number)` identity resolves; several stay
//! ambiguous (even when the numbers match across scopes), and any
//! unreadable store degrades to `None` so callers keep the legacy Git
//! binding.

use super::support::*;
use super::*;
use crate::worktree::active_link::{ActiveLink, repo_key_for_checkout, resolve_active_link_issue};

fn seed_link(
    storage: &Storage,
    repo_key: &str,
    branch: &str,
    provider: &str,
    project: &str,
    issue: u64,
) {
    crate::branch_links::ensure_schema(&storage.connection).expect("link schema");
    let key = crate::branch_links::IssueKey::new(provider, project, issue.to_string())
        .expect("issue key");
    crate::branch_links::store::link(
        &storage.connection,
        &crate::branch_links::LinkParams {
            repo_key,
            branch,
            issue: &key,
            issue_number: issue,
            source: "test",
            now: now_unix_secs().max(1),
        },
    )
    .expect("link must insert");
}

#[test]
fn single_link_resolves_and_several_stay_ambiguous() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("link-ownership");
    seed_link(&storage, "forge.example/r", "feat/1", "redmine", "p", 11);
    assert_eq!(
        resolve_active_link_issue(&storage.connection, "forge.example/r", "feat/1"),
        ActiveLink::Single(11)
    );
    seed_link(&storage, "forge.example/r", "feat/1", "redmine", "p", 12);
    assert_eq!(
        resolve_active_link_issue(&storage.connection, "forge.example/r", "feat/1"),
        ActiveLink::Ambiguous,
        "several distinct numbers are never guessed"
    );
    assert_eq!(
        resolve_active_link_issue(&storage.connection, "forge.example/r", "feat/other"),
        ActiveLink::None,
        "unlinked branches resolve to none"
    );
    drop(temp);
}

#[test]
fn cross_scope_same_number_is_ambiguous() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("link-detached");
    // Same number under two scopes is two distinct identities (issue
    // 628): they must never collapse into one active issue.
    seed_link(&storage, "forge.example/r", "feat/1", "redmine", "p", 11);
    seed_link(&storage, "forge.example/r", "feat/1", "forgejo", "o/r", 11);
    assert_eq!(
        resolve_active_link_issue(&storage.connection, "forge.example/r", "feat/1"),
        ActiveLink::Ambiguous,
        "distinct scopes sharing a number are never guessed"
    );
    // Detaching both links returns the branch to none.
    crate::branch_links::store::detach(
        &storage.connection,
        "forge.example/r",
        "feat/1",
        &crate::branch_links::IssueKey::new("redmine", "p", "11").expect("key"),
        "unbind",
        now_unix_secs().max(1),
    )
    .expect("detach redmine row");
    crate::branch_links::store::detach(
        &storage.connection,
        "forge.example/r",
        "feat/1",
        &crate::branch_links::IssueKey::new("forgejo", "o/r", "11").expect("key"),
        "unbind",
        now_unix_secs().max(1),
    )
    .expect("detach forgejo row");
    assert_eq!(
        resolve_active_link_issue(&storage.connection, "forge.example/r", "feat/1"),
        ActiveLink::None,
        "detached history never decides"
    );
    drop(temp);
}

#[test]
fn same_scope_duplicate_external_ids_resolve_single() {
    // Two rows in one scope that agree on the number are one identity
    // (e.g. external ids "11" and "011"), so they still resolve.
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("link-same-scope");
    crate::branch_links::ensure_schema(&storage.connection).expect("link schema");
    for external_id in ["11", "011"] {
        let key = crate::branch_links::IssueKey::new("redmine", "p", external_id).expect("key");
        crate::branch_links::store::link(
            &storage.connection,
            &crate::branch_links::LinkParams {
                repo_key: "forge.example/r",
                branch: "feat/1",
                issue: &key,
                issue_number: 11,
                source: "test",
                now: now_unix_secs().max(1),
            },
        )
        .expect("link must insert");
    }
    assert_eq!(
        resolve_active_link_issue(&storage.connection, "forge.example/r", "feat/1"),
        ActiveLink::Single(11),
        "one scope plus one number is unambiguous"
    );
    drop(temp);
}

#[test]
fn missing_table_and_blank_inputs_resolve_to_none() {
    let _lock = lock_workflow_tests();
    let (temp, storage, _env) = open_temp_db("link-missing");
    // Deliberately no branch-link schema: pre-P3 databases must fall
    // back to the legacy binding instead of erroring the acquire.
    assert_eq!(
        resolve_active_link_issue(&storage.connection, "forge.example/r", "feat/1"),
        ActiveLink::None
    );
    crate::branch_links::ensure_schema(&storage.connection).expect("link schema");
    assert_eq!(
        resolve_active_link_issue(&storage.connection, "", "feat/1"),
        ActiveLink::None
    );
    assert_eq!(
        resolve_active_link_issue(&storage.connection, "forge.example/r", ""),
        ActiveLink::None
    );
    drop(temp);
}

#[test]
fn checkout_keys_prefer_canonical_origin_with_local_fallback() {
    let _lock = lock_workflow_tests();
    let dir = unique_cache("link-key");
    let key = repo_key_for_checkout(
        &FakeWorktreeRunner::new(vec![FakeResponse {
            args: vec![
                "remote".to_string(),
                "get-url".to_string(),
                "origin".to_string(),
            ],
            status: 0,
            stdout: "git@forge.example:owner/repo.git".to_string(),
        }]),
        dir.path(),
    )
    .expect("canonical origin must key");
    assert_eq!(key, "forge.example/owner/repo");
    // No origin: local-only fallback keyed by the checkout path.
    let fallback = repo_key_for_checkout(
        &FakeWorktreeRunner::new(vec![FakeResponse {
            args: vec![
                "remote".to_string(),
                "get-url".to_string(),
                "origin".to_string(),
            ],
            status: 128,
            stdout: String::new(),
        }]),
        dir.path(),
    )
    .expect("missing origin must fall back");
    assert!(
        fallback.starts_with("local:"),
        "fallback must be marked local-only: {fallback}"
    );
    // Unparseable origin: no key at all rather than a guessed one.
    assert!(
        repo_key_for_checkout(
            &FakeWorktreeRunner::new(vec![FakeResponse {
                args: vec![
                    "remote".to_string(),
                    "get-url".to_string(),
                    "origin".to_string()
                ],
                status: 0,
                stdout: "::::".to_string(),
            }]),
            dir.path(),
        )
        .is_none(),
        "garbage origins must not produce keys"
    );
    drop(dir);
}
