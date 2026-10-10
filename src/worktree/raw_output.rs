//! A [`WorktreeRunner`] that returns its command's stdout verbatim.
//!
//! [`super::ProcessWorktreeRunner`] deliberately strips control
//! characters and bounds its echo to `MAX_ECHO_CHARS` so arbitrary
//! repository output cannot flood logs or leak into errors. That is the
//! right default and the wrong shape for `git worktree list --porcelain`,
//! whose newline separated records cannot survive it, so this narrow
//! runner exists only to produce the input
//! [`crate::worktree::parse_worktree_list`] parses.
//!
//! The narrowing is deliberate and stays narrow: argv is still passed as
//! an array (no shell), and an oversized listing is still refused rather
//! than truncated, because a truncated porcelain listing would drop
//! worktrees without saying so.

use std::path::Path;
use std::process::Command;

use super::{GitOutput, WorktreeError, WorktreeRunner};

/// Ceiling on a porcelain listing handed back by [`RawOutputRunner`].
/// Far more worktrees than a repository holds in practice, and a hard
/// stop so the read cannot grow without bound.
const MAX_PORCELAIN_BYTES: usize = 64 * 1024;

pub struct RawOutputRunner;

impl WorktreeRunner for RawOutputRunner {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<GitOutput, WorktreeError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(workdir)
            .output()
            .map_err(|error| WorktreeError::new("git", format!("could not run git: {error}")))?;
        if output.stdout.len() > MAX_PORCELAIN_BYTES {
            return Err(WorktreeError::new(
                "git",
                "git worktree list output exceeded its bound",
            ));
        }
        Ok(GitOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }
}
