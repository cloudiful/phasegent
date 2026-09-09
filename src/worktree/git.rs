//! Thin `git` wrappers in `branch_context`-style. The functions take
//! a [`WorktreeRunner`] (testable via `FakeWorktreeRunner`) and a
//! per-call working directory, and return structured
//! [`WorktreeError`] values on non-zero exits. Phase 1 only uses
//! `worktree_add` / `worktree_remove` from tests with temp repos; the
//! production call site for prune is Phase 2.

use std::path::Path;

use crate::worktree::{WorktreeError, WorktreeListEntry, WorktreeRunner, bounded};

/// `git worktree add <path> -b <branch> HEAD` wrapper. The new
/// branch and the new worktree directory are created from the
/// current `HEAD` of `repo_path`. Failure exits are surfaced as
/// structured errors; stdout is sanitised.
#[allow(dead_code)]
pub fn worktree_add(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    target: &Path,
    branch: &str,
) -> Result<(), WorktreeError> {
    let target_str = target.to_string_lossy();
    let output = runner.run(
        &["worktree", "add", &target_str, "-b", branch, "HEAD"],
        repo_path,
    )?;
    if output.status != 0 {
        return Err(WorktreeError::new(
            "git",
            format!("git worktree add failed with exit status {}", output.status),
        ));
    }
    Ok(())
}

/// `git worktree remove <path>` wrapper. The `git` invocation is run
/// from `repo_root` (the canonical repo working directory) so the
/// command is robust against a missing or already-removed target —
/// `git worktree remove` resolves the worktree by path relative to
/// the repository's metadata, and Phase 2 prune also passes the
/// resolved repo root as `repo_root` for the same reason. Phase 1
/// used `target` as the workdir; the Phase 2 P3 fix takes the repo
/// root instead.
#[allow(dead_code)]
pub fn worktree_remove(
    runner: &dyn WorktreeRunner,
    repo_root: &Path,
    target: &Path,
) -> Result<(), WorktreeError> {
    let target_str = target.to_string_lossy();
    let output = runner.run(&["worktree", "remove", &target_str], repo_root)?;
    if output.status != 0 {
        return Err(WorktreeError::new(
            "git",
            format!(
                "git worktree remove failed with exit status {}",
                output.status
            ),
        ));
    }
    Ok(())
}

/// Dirty probe used by the Phase 2 prune gate. Empty `git status
/// --porcelain` output is `Ok(true)` (clean); any line means
/// `Ok(false)`. Git errors are returned as `WorktreeError` so the
/// caller can decide whether dirty / unknown should block prune.
#[allow(dead_code)]
pub fn is_clean(runner: &dyn WorktreeRunner, worktree_path: &Path) -> Result<bool, WorktreeError> {
    let output = runner.run(&["status", "--porcelain"], worktree_path)?;
    if output.status != 0 {
        return Err(WorktreeError::new(
            "git",
            format!("git status failed with exit status {}", output.status),
        ));
    }
    Ok(output.stdout.trim().is_empty())
}

/// `git symbolic-ref --quiet --short HEAD` wrapper used by the
/// `no_conflict` acquire path. Detached HEAD is reported as a
/// structured `branch` error so the caller can either check out a
/// named branch or treat it as a Phase 2 follow-up.
pub fn current_branch_for(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
) -> Result<String, WorktreeError> {
    let output = runner.run(&["symbolic-ref", "--quiet", "--short", "HEAD"], repo_path)?;
    if output.status != 0 {
        return Err(WorktreeError::new(
            "branch",
            "HEAD is detached; pick a named branch before acquire",
        ));
    }
    let trimmed = output.stdout.trim().to_string();
    if trimmed.is_empty() {
        return Err(WorktreeError::new(
            "branch",
            "HEAD is detached; pick a named branch before acquire",
        ));
    }
    Ok(bounded(&trimmed))
}

/// Parse `git worktree list --porcelain` output into a flat list of
/// entries. The parser is tolerant of extra fields so future Git
/// versions can add columns without breaking the module.
#[allow(dead_code)]
pub fn parse_worktree_list(raw: &str) -> Vec<WorktreeListEntry> {
    let mut entries: Vec<WorktreeListEntry> = Vec::new();
    let mut current: Option<WorktreeListEntry> = None;
    for line in raw.lines() {
        if line.is_empty() {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }
        let mut parts = line.splitn(3, ' ');
        let key = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("");
        match key {
            "worktree" => {
                if let Some(entry) = current.take() {
                    entries.push(entry);
                }
                current = Some(WorktreeListEntry {
                    worktree: value.to_owned(),
                    head: None,
                    branch: None,
                });
            }
            "HEAD" => {
                if let Some(entry) = current.as_mut() {
                    entry.head = Some(value.to_owned());
                }
            }
            "branch" => {
                if let Some(entry) = current.as_mut() {
                    entry.branch = Some(value.to_owned());
                }
            }
            _ => {}
        }
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    entries
}
