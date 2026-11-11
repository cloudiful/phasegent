//! Per-repository advisory operation locks.
//!
//! Cooperating phasegent operations acquire one stable lock file for the
//! canonical repository before they read current occupancy and keep it until
//! their Git mutation and short registry transaction have finished. The file
//! is opened read+write, is never unlinked, and the OS releases the lock when
//! the handle drops, including on a crash. The protection covers cooperating
//! phasegent processes only; arbitrary Git or host actions starting after a
//! check are outside it. Git keeps its own worktree locks.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use crate::worktree::WorktreeError;

pub(crate) struct OperationLock {
    file: Option<File>,
    path: PathBuf,
}

impl OperationLock {
    #[allow(dead_code)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for OperationLock {
    fn drop(&mut self) {
        // Dropping the file handle releases the OS advisory lock. The lock
        // file itself is intentionally left in place: unlinking it would let
        // two processes lock two different inodes for the same repository.
        self.file.take();
    }
}

/// Acquire the single advisory lock for one canonical repository identity.
/// Contention is bounded and actionable; there is no retry and no bypass.
pub(crate) fn acquire(repo_identity: &str) -> Result<OperationLock, WorktreeError> {
    let path = lock_path(repo_identity)?;
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            WorktreeError::new(
                "storage",
                format!("could not open the worktree operation lock: {error}"),
            )
        })?;
    match file.try_lock() {
        Ok(()) => Ok(OperationLock {
            file: Some(file),
            path,
        }),
        Err(std::fs::TryLockError::WouldBlock) => Err(WorktreeError::new(
            "busy",
            "another phasegent worktree operation is in progress for this repository; retry after it finishes",
        )),
        Err(std::fs::TryLockError::Error(error)) => Err(WorktreeError::new(
            "storage",
            format!("could not lock the worktree operation file: {error}"),
        )),
    }
}

fn lock_path(repo_identity: &str) -> Result<PathBuf, WorktreeError> {
    let base = super::naming::cache_base_dir()
        .ok_or_else(|| WorktreeError::new("storage", "could not resolve phasegent cache dir"))?;
    let fingerprint = super::naming::compute_fingerprint(repo_identity);
    Ok(base.join("worktrees").join(format!("{fingerprint}.lock")))
}

fn create_private_dir(path: &std::path::Path) -> Result<(), WorktreeError> {
    std::fs::create_dir_all(path).map_err(|error| {
        WorktreeError::new(
            "storage",
            format!("could not create the worktree lock directory: {error}"),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}
