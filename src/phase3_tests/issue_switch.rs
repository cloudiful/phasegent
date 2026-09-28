//! P4 attempt-4 focused tests: repo-wide switch gating, explicit-branch
//! durable links, and conventional-default protection (issue 628).
//!
//! Shared fixtures stay in this module; only new coverage lives in the
//! child modules. Every test runs against real temp repositories with
//! the database pinned to a temp file under the workflow lock; the live
//! store is never touched. Only local Git commands run; no network, no
//! stash, no deletes.

use crate::branch_context::GitRunner;
use crate::lifecycle;
use std::path::{Path, PathBuf};

#[path = "issue_switch/default_protection.rs"]
mod default_protection;
#[path = "issue_switch/explicit_link.rs"]
mod explicit_link;
#[path = "issue_switch/switch_gates.rs"]
mod switch_gates;

/// Temp repo with one commit on `main` and NO origin, so cached
/// default-branch detection is unknown. Returns the repo plus a
/// scratch dir for the pinned database.
fn bare_repo(tag: &str) -> Option<(super::TempRepo, PathBuf)> {
    let repo = super::TempRepo::new(tag)?;
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
        "phasegent-phase3-barerepo-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&db).expect("scratch dir must create");
    Some((repo, db))
}

/// Filesystem fallback key for a repo without origin, resolved through
/// git itself so it matches what the link helpers compute.
fn fallback_key(repo: &super::TempRepo) -> String {
    let output = repo
        .runner()
        .run(&["rev-parse", "--show-toplevel"])
        .expect("toplevel resolves");
    assert_eq!(output.status, 0);
    crate::branch_links::resolve_repo_key(None, Path::new(output.stdout.trim()))
        .expect("fallback key")
        .key
}

fn branch_exists(repo: &super::TempRepo, branch: &str) -> bool {
    repo.runner()
        .run(&[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .is_ok_and(|output| output.status == 0)
}

fn linked_entries(
    db: &Path,
    repo_key: &str,
    branch: &str,
) -> Vec<crate::branch_links::LinkedIssue> {
    let storage = crate::infra::storage::Storage::open_at(&db.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    crate::branch_links::issues_for_branch(
        &storage.connection,
        repo_key,
        branch,
        false,
        &crate::branch_links::UnknownState,
    )
    .expect("read must work")
}

fn seed_link(db: &Path, repo_key: &str, branch: &str, provider: &str, project: &str, issue: u64) {
    let storage = crate::infra::storage::Storage::open_at(&db.join("phasegent.sqlite3"))
        .expect("temp storage must open");
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
            now: crate::worktree::now_unix_secs().max(1),
        },
    )
    .expect("link must insert");
}

fn insert_active_lease(
    storage: &crate::infra::storage::Storage,
    identity: &str,
    lease_id: &str,
    issue: u64,
    session: &str,
    path: &str,
    status: &str,
) {
    use crate::worktree::leases::{NewLease, insert_lease};
    let now = crate::worktree::now_unix_secs();
    insert_lease(
        storage,
        NewLease {
            lease_id,
            identity,
            issue,
            session,
            checkout_path: path,
            worktree_path: path,
            branch: "phasegent/1-aaaaaa",
            status,
            created_at: now,
            heartbeat_at: now,
        },
    )
    .expect("lease must insert");
}

fn repo_lease_identity(repo: &super::TempRepo) -> String {
    crate::worktree::repo_identity(&crate::worktree::ProcessWorktreeRunner::new(), &repo.0)
        .expect("lease identity must resolve")
}

fn switch_params_for<'a>(
    repo: &'a super::TempRepo,
    issue_id: u64,
    branch: &'a str,
    project: Option<&'a str>,
) -> lifecycle::IssueSwitchParams<'a> {
    lifecycle::IssueSwitchParams {
        repo_path: &repo.0,
        issue_id,
        branch,
        base: None,
        scope_provider: "redmine",
        scope_project: project,
        explicit_repository: None,
        session: None,
    }
}

fn explicit_params_for<'a>(
    issue_id: u64,
    branch: &'a str,
    project: Option<&'a str>,
) -> lifecycle::ExplicitLinkParams<'a> {
    lifecycle::ExplicitLinkParams {
        issue_id,
        branch,
        base: None,
        scope_provider: "redmine",
        scope_project: project,
        explicit_repository: None,
    }
}
