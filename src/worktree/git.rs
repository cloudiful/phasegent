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
    worktree_add_from(runner, repo_path, target, branch, "HEAD")
}

/// `git worktree add <path> -b <branch> <base>` wrapper. `base` is any
/// revision Git resolves to a commit (a branch, tag, sha, or
/// `origin/main`); the caller validates it before this runs so a bad
/// ref never reaches `git worktree add`.
#[allow(dead_code)]
pub fn worktree_add_from(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    target: &Path,
    branch: &str,
    base: &str,
) -> Result<(), WorktreeError> {
    let target_str = target.to_string_lossy();
    let output = runner.run(
        &["worktree", "add", &target_str, "-b", branch, base],
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

/// True when `reference` resolves to a commit in `repo_path`.
///
/// `git rev-parse --verify --quiet <ref>^{commit}` is a network-free,
/// read-only probe used before `worktree add` so an explicit `--base`
/// that does not exist fails without creating a branch, a worktree
/// directory, or a lease row.
pub fn ref_resolves_to_commit(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    reference: &str,
) -> Result<bool, WorktreeError> {
    let spec = format!("{reference}^{{commit}}");
    let output = runner.run(&["rev-parse", "--verify", "--quiet", &spec], repo_path)?;
    Ok(output.status == 0)
}

/// True when `path` is inside a Git work tree. A non-repository or an
/// unreadable path is `Ok(false)` rather than an error so a read-only
/// probe can report "not a Git worktree" structurally.
pub fn is_inside_work_tree(
    runner: &dyn WorktreeRunner,
    path: &Path,
) -> Result<bool, WorktreeError> {
    let output = runner.run(&["rev-parse", "--is-inside-work-tree"], path)?;
    if output.status != 0 {
        return Ok(false);
    }
    Ok(output.stdout.trim() == "true")
}

/// Resolve `HEAD` to its commit sha, or `None` on an unborn branch (a
/// fresh repository with no commit) or any failure. Read-only.
pub fn head_rev(runner: &dyn WorktreeRunner, path: &Path) -> Result<Option<String>, WorktreeError> {
    let output = runner.run(&["rev-parse", "--verify", "--quiet", "HEAD"], path)?;
    if output.status != 0 {
        return Ok(None);
    }
    let trimmed = output.stdout.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(bounded(trimmed)))
    }
}

/// `--git-dir` and `--git-common-dir` for `path`, each resolved to an
/// absolute path. For the main checkout the two are equal; a linked
/// worktree has a per-worktree `--git-dir` under the common dir, which
/// is how a read-only probe distinguishes the main checkout.
pub fn checkout_git_dirs(
    runner: &dyn WorktreeRunner,
    path: &Path,
) -> Result<(String, String), WorktreeError> {
    let git_dir = runner.run(&["rev-parse", "--git-dir"], path)?;
    let common = runner.run(&["rev-parse", "--git-common-dir"], path)?;
    if git_dir.status != 0 || common.status != 0 {
        return Err(WorktreeError::new(
            "git",
            "git rev-parse --git-dir/--git-common-dir failed",
        ));
    }
    Ok((
        absolute_git_dir(path, git_dir.stdout.trim()),
        absolute_git_dir(path, common.stdout.trim()),
    ))
}

/// Resolve a `git rev-parse --git-dir` style value against `base` and
/// canonicalise it so `.` and its absolute form compare equal.
fn absolute_git_dir(base: &Path, raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let candidate = Path::new(raw);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    let canonical = resolved.canonicalize().unwrap_or(resolved);
    bounded(&canonical.to_string_lossy())
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
