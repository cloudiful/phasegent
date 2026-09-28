//! P4 safe same-checkout create/link/switch tests (issue 628).
//!
//! Shared fixtures stay in this module; only new coverage lives in the
//! child modules. Every test runs against real temp repositories with
//! the database pinned to a temp file under the workflow lock; the live
//! store is never touched. Only local Git commands run; no network, no
//! stash, no deletes.

use crate::branch_context::GitRunner;
use crate::infra::storage::test_support::EnvGuard;
use crate::lifecycle;
use std::path::{Path, PathBuf};

mod provider_scopes;
mod switch_flow;
mod switch_guards;
mod switch_leases;

/// Commit-ready repo on `main` with an origin whose cached HEAD marks
/// `main` as the default branch.
pub(super) fn switch_repo(tag: &str) -> Option<(super::TempRepo, PathBuf)> {
    let repo = super::TempRepo::new(tag)?;
    let git = |args: &[&str]| {
        let output = repo.runner().run(args).expect("git must run");
        assert_eq!(output.status, 0, "git {args:?} must succeed");
    };
    git(&[
        "-c",
        "user.name=phasegent-test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--allow-empty",
        "-q",
        "-m",
        "init",
    ]);
    git(&["checkout", "-q", "-B", "main"]);
    repo.set_origin("https://git.example/acme/widgets.git");
    git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    let db = crate::test_scratch::root().join(format!(
        "phasegent-phase3-switch-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&db).expect("scratch dir must create");
    Some((repo, db))
}

pub(super) fn pin_temp_db(dir: &Path) -> EnvGuard {
    EnvGuard::set(
        "PHASEGENT_DB_PATH",
        dir.join("phasegent.sqlite3")
            .as_os_str()
            .to_string_lossy()
            .as_ref(),
    )
}

fn switch_params<'a>(
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

pub(super) fn current_branch(repo: &super::TempRepo) -> String {
    let output = repo
        .runner()
        .run(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .expect("branch resolves");
    assert_eq!(output.status, 0);
    output.stdout.trim().to_owned()
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

fn linked_numbers(db: &Path, repo_key: &str, branch: &str) -> Vec<u64> {
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
    .into_iter()
    .map(|entry| entry.issue_number)
    .collect()
}

pub(super) fn canonical_key() -> String {
    crate::branch_links::repo_key_for_origin("https://git.example/acme/widgets.git")
        .expect("canonical key")
}
