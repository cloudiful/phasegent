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
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::{Scratch, expect_json};

/// Create one local-provider issue through the binary and return its number.
pub fn create_local_issue(scratch: &Scratch, cwd: &Path, title: &str, body: &str) -> u64 {
    let document = expect_json(
        scratch,
        cwd,
        &[
            "--role",
            "orchestrator",
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
        &[
            "--role",
            "orchestrator",
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

/// Resolve the repository identity production scopes lease work to, and let
/// the production code create the lease table: `worktree list` opens the store
/// through the same `ensure_schema` every lease path uses, so a fixture never
/// duplicates the production DDL.
pub fn open_lease_store(scratch: &Scratch, repo: &Path) -> String {
    let repo = repo.to_str().expect("utf8 repo path");
    let document = expect_json(
        scratch,
        scratch.root(),
        &[
            "--role",
            "executor",
            "worktree",
            "list",
            "--repo",
            repo,
            "--no-sync",
        ],
    );
    document["repo_identity"]
        .as_str()
        .unwrap_or_else(|| panic!("worktree list returned no repo identity: {document}"))
        .to_owned()
}

/// Insert one `active` lease row, mirroring the columns `insert_lease` writes.
pub fn insert_active_lease(
    scratch: &Scratch,
    identity: &str,
    issue: u64,
    session: &str,
    checkout: &Path,
    worktree: &Path,
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after the epoch")
        .as_secs() as i64;
    let connection =
        Connection::open(scratch.join("phasegent.sqlite3")).expect("open the lease database");
    connection
        .execute(
            "INSERT INTO worktree_leases \
             (lease_id, repo_identity, issue, session, checkout_path, worktree_path, branch, \
              status, created_at, heartbeat_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                format!("snapshots-lease-{issue}-{session}"),
                identity,
                issue as i64,
                session,
                checkout.to_str().expect("utf8 checkout path"),
                worktree.to_str().expect("utf8 worktree path"),
                "main",
                "active",
                now,
                now,
            ],
        )
        .expect("insert lease row");
}
