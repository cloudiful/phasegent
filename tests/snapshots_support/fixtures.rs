#![allow(dead_code)]
//! Provider, git, and lease fixtures for the `snapshots_*` integration tests.
//!
//! Everything here writes only inside the caller's [`Scratch`]: the local
//! provider issues are created through the compiled binary, the git objects
//! live in a scratch checkout, and the lease rows land in the scratch lease
//! database.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;

use super::{Scratch, expect_json};

/// Create one local-provider issue through the binary and return its number.
pub fn create_local_issue(scratch: &Scratch, cwd: &Path, title: &str, body: &str) -> u64 {
    let document = expect_json(
        scratch,
        cwd,
        Some("orchestrator"),
        &[
            "--provider",
            "local",
            "issue",
            "create",
            "--title",
            title,
            "--body",
            body,
        ],
    );
    document["number"]
        .as_u64()
        .unwrap_or_else(|| panic!("issue create returned no number: {document}"))
}

/// Move a local-provider issue to `status` (the local provider validates the
/// name against its static catalogue, so a typo fails in the fixture).
pub fn set_local_status(scratch: &Scratch, cwd: &Path, number: u64, status: &str) {
    let number = number.to_string();
    expect_json(
        scratch,
        cwd,
        Some("orchestrator"),
        &[
            "--provider",
            "local",
            "status",
            "set",
            &number,
            "--status",
            status,
        ],
    );
}

/// Initialise a scratch git checkout with one commit and return its path.
pub fn init_repo(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).expect("create repo dir");
    let init = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir)
        .output()
        .expect("git init runs");
    assert!(
        init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&init.stderr),
    );
    let commit = Command::new("git")
        .args([
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
        .current_dir(dir)
        .output()
        .expect("git commit runs");
    assert!(
        commit.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&commit.stderr),
    );
    dir.to_path_buf()
}

/// Add one linked worktree on a new branch and return its directory.
pub fn add_worktree(repo: &Path, dir: &Path, branch: &str) -> PathBuf {
    let output = Command::new("git")
        .arg("worktree")
        .arg("add")
        .arg(dir)
        .args(["-b", branch, "HEAD"])
        .current_dir(repo)
        .output()
        .expect("git worktree add runs");
    assert!(
        output.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    dir.to_path_buf()
}

/// Resolve the repository identity the registry scopes worktree work to.
pub fn open_registry_store(scratch: &Scratch, repo: &Path) -> String {
    let repo = repo.to_str().expect("utf8 repo path");
    let document = expect_json(
        scratch,
        scratch.root(),
        Some("executor"),
        &["worktree", "list", "--repo", repo, "--no-sync"],
    );
    document["repo_identity"]
        .as_str()
        .unwrap_or_else(|| panic!("worktree list returned no repo identity: {document}"))
        .to_owned()
}

/// Insert one directory record with one legacy issue association and one
/// session, creating the registry tables the production reader expects.
pub fn insert_registry_directory(
    scratch: &Scratch,
    identity: &str,
    issue: u64,
    session: &str,
    checkout: &Path,
    worktree: &Path,
    branch: &str,
) {
    let connection =
        Connection::open(scratch.join("phasegent.sqlite3")).expect("open the registry database");
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS worktree_registry_meta (
                 name TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS worktree_directories (
                 directory_id TEXT PRIMARY KEY,
                 repo_identity TEXT NOT NULL,
                 checkout_path TEXT NOT NULL,
                 worktree_path TEXT NOT NULL,
                 branch TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 UNIQUE (repo_identity, worktree_path)
             );
             CREATE TABLE IF NOT EXISTS worktree_directory_tasks (
                 directory_id TEXT NOT NULL,
                 task_id INTEGER NOT NULL,
                 issue INTEGER NOT NULL,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (directory_id, task_id)
             );
             CREATE TABLE IF NOT EXISTS worktree_directory_legacy_issues (
                 directory_id TEXT NOT NULL,
                 issue INTEGER NOT NULL,
                 provider TEXT NOT NULL DEFAULT '',
                 project TEXT NOT NULL DEFAULT '',
                 external_id INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (directory_id, issue, provider, project, external_id)
             );
             CREATE TABLE IF NOT EXISTS worktree_directory_sessions (
                 directory_id TEXT NOT NULL,
                 session TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (directory_id, session)
             );",
        )
        .expect("create registry tables");
    connection
        .execute(
            "INSERT OR IGNORE INTO worktree_registry_meta (name, value) VALUES ('schema_version', '1')",
            [],
        )
        .expect("record registry version");
    let directory_id = format!("dir-{issue}-{session}");
    connection
        .execute(
            "INSERT INTO worktree_directories \
             (directory_id, repo_identity, checkout_path, worktree_path, branch, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            rusqlite::params![
                directory_id.clone(),
                identity,
                checkout.to_str().expect("utf8 checkout path"),
                worktree.to_str().expect("utf8 worktree path"),
                branch,
            ],
        )
        .expect("insert directory row");
    connection
        .execute(
            "INSERT INTO worktree_directory_legacy_issues \
             (directory_id, issue, provider, project, external_id, created_at) \
             VALUES (?1, ?2, '', '', 0, 1)",
            rusqlite::params![directory_id.clone(), issue as i64],
        )
        .expect("insert legacy issue row");
    connection
        .execute(
            "INSERT INTO worktree_directory_sessions (directory_id, session, created_at) \
             VALUES (?1, ?2, 1)",
            rusqlite::params![directory_id, session],
        )
        .expect("insert session row");
}
