//! Narrow Git operations for close-time checkout restoration.
//!
//! Every command runs in the verified main checkout and is argument-safe:
//! discrete argv entries, no shell, no force, no tag fetch, and no reset,
//! stash, clean, branch deletion, or merge commit.

use std::path::Path;

use crate::worktree::{WorktreeRunner, bounded};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Upstream {
    Configured {
        remote: String,
        merge_ref: String,
        branch: String,
    },
    None,
    Unsupported(String),
}

fn validate_remote(remote: &str) -> Result<(), String> {
    if remote.trim().is_empty() {
        return Err("remote name is empty".to_owned());
    }
    if remote.starts_with('-') || remote.chars().any(char::is_control) {
        return Err("remote name is not a safe Git argument".to_owned());
    }
    Ok(())
}

fn validate_merge_ref(merge_ref: &str) -> Result<String, String> {
    let Some(branch) = merge_ref.strip_prefix("refs/heads/") else {
        return Err("configured upstream is not a local branch ref".to_owned());
    };
    if branch.trim().is_empty() || branch.starts_with('-') || branch.chars().any(char::is_control) {
        return Err("configured upstream branch is unusable".to_owned());
    }
    Ok(branch.to_owned())
}

/// Read `branch.<branch>.remote` and `.merge` and validate that they name a
/// usable configured remote-tracking upstream. A missing configuration is
/// [`Upstream::None`]; an explicitly local (`.`) or otherwise unsupported
/// upstream is [`Upstream::Unsupported`] so the caller restores only.
pub(crate) fn resolve_upstream(
    worktree: &dyn WorktreeRunner,
    main_checkout: &Path,
    branch: &str,
) -> Result<Upstream, String> {
    if branch.trim().is_empty() || branch.starts_with('-') || branch.chars().any(char::is_control) {
        return Err("baseline branch is unusable for upstream lookup".to_owned());
    }
    let remote = match worktree.run(
        &["config", "--get", &format!("branch.{branch}.remote")],
        main_checkout,
    ) {
        Ok(output) if output.status == 0 => output.stdout.trim().to_owned(),
        Ok(output) if output.status == 1 => return Ok(Upstream::None),
        Ok(output) => {
            return Err(format!(
                "could not read branch remote (exit status {})",
                output.status
            ));
        }
        Err(error) => return Err(bounded(&error.message)),
    };
    let merge_ref = match worktree.run(
        &["config", "--get", &format!("branch.{branch}.merge")],
        main_checkout,
    ) {
        Ok(output) if output.status == 0 => output.stdout.trim().to_owned(),
        Ok(output) if output.status == 1 => {
            return Ok(Upstream::Unsupported(
                "branch has a remote but no configured merge ref".to_owned(),
            ));
        }
        Ok(output) => {
            return Err(format!(
                "could not read branch merge ref (exit status {})",
                output.status
            ));
        }
        Err(error) => return Err(bounded(&error.message)),
    };
    if remote.is_empty() {
        return Ok(Upstream::None);
    }
    if let Err(reason) = validate_remote(&remote) {
        return Ok(Upstream::Unsupported(reason));
    }
    if remote == "." {
        return Ok(Upstream::Unsupported(
            "local-only upstream is not fetched or updated".to_owned(),
        ));
    }
    let branch_name = match validate_merge_ref(&merge_ref) {
        Ok(branch) => branch,
        Err(reason) => return Ok(Upstream::Unsupported(reason)),
    };
    match worktree.run(
        &["config", "--get", &format!("remote.{remote}.url")],
        main_checkout,
    ) {
        Ok(output) if output.status == 0 && !output.stdout.trim().is_empty() => {}
        Ok(output) if output.status == 0 || output.status == 1 => {
            return Ok(Upstream::Unsupported(format!(
                "remote '{remote}' has no configured URL"
            )));
        }
        Ok(output) => {
            return Err(format!(
                "could not read remote URL (exit status {})",
                output.status
            ));
        }
        Err(error) => return Err(bounded(&error.message)),
    }
    Ok(Upstream::Configured {
        remote,
        merge_ref,
        branch: branch_name,
    })
}

/// Fetch exactly the configured remote branch without tags and return the
/// fetched commit. `FETCH_HEAD` is the only local state written by the fetch.
pub(crate) fn fetch_upstream(
    worktree: &dyn WorktreeRunner,
    main_checkout: &Path,
    remote: &str,
    merge_ref: &str,
) -> Result<String, String> {
    validate_remote(remote)?;
    if merge_ref.trim().is_empty() || merge_ref.starts_with('-') {
        return Err("upstream ref is not a safe Git argument".to_owned());
    }
    match worktree.run(&["fetch", "--no-tags", remote, merge_ref], main_checkout) {
        Ok(output) if output.status == 0 => {}
        Ok(output) => {
            return Err(format!(
                "fetching '{remote} {merge_ref}' failed with exit status {}",
                output.status
            ));
        }
        Err(error) => return Err(bounded(&error.message)),
    }
    match worktree.run(&["rev-parse", "--verify", "FETCH_HEAD"], main_checkout) {
        Ok(output) if output.status == 0 && !output.stdout.trim().is_empty() => {
            Ok(output.stdout.trim().to_owned())
        }
        Ok(output) => Err(format!(
            "fetched upstream commit could not be resolved (exit status {})",
            output.status
        )),
        Err(error) => Err(bounded(&error.message)),
    }
}

pub(crate) fn is_ancestor(
    worktree: &dyn WorktreeRunner,
    main_checkout: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, String> {
    match worktree.run(
        &["merge-base", "--is-ancestor", ancestor, descendant],
        main_checkout,
    ) {
        Ok(output) if output.status == 0 => Ok(true),
        Ok(output) if output.status == 1 => Ok(false),
        Ok(output) => Err(format!(
            "ancestry check failed with exit status {}",
            output.status
        )),
        Err(error) => Err(bounded(&error.message)),
    }
}

fn validate_branch(branch: &str) -> Result<&str, String> {
    if branch.trim().is_empty() || branch.starts_with('-') || branch.chars().any(char::is_control) {
        return Err("baseline branch is not a safe Git argument".to_owned());
    }
    Ok(branch)
}

/// True when the captured branch exists as a local `refs/heads` ref. Checked
/// immediately before switching so a deleted branch is a skip, not a DWIM
/// recreation from a remote-tracking namesake.
pub(crate) fn local_branch_exists(
    worktree: &dyn WorktreeRunner,
    main_checkout: &Path,
    branch: &str,
) -> Result<bool, String> {
    let branch = validate_branch(branch)?;
    let reference = format!("refs/heads/{branch}");
    match worktree.run(
        &["show-ref", "--verify", "--quiet", &reference],
        main_checkout,
    ) {
        Ok(output) if output.status == 0 => Ok(true),
        Ok(output) if output.status == 1 => Ok(false),
        Ok(output) => Err(format!(
            "local branch check failed with exit status {}",
            output.status
        )),
        Err(error) => Err(bounded(&error.message)),
    }
}

pub(crate) fn checkout(
    worktree: &dyn WorktreeRunner,
    main_checkout: &Path,
    branch: &str,
) -> Result<(), String> {
    let branch = validate_branch(branch)?;
    // `--no-guess` disables remote-tracking DWIM, so a branch deleted after
    // the existence probe fails the switch instead of being recreated.
    match worktree.run(&["switch", "--no-guess", branch], main_checkout) {
        Ok(output) if output.status == 0 => Ok(()),
        Ok(output) => Err(format!(
            "switch to '{branch}' failed with exit status {}",
            output.status
        )),
        Err(error) => Err(bounded(&error.message)),
    }
}

pub(crate) fn fast_forward(
    worktree: &dyn WorktreeRunner,
    main_checkout: &Path,
    target: &str,
) -> Result<(), String> {
    match worktree.run(&["merge", "--ff-only", target], main_checkout) {
        Ok(output) if output.status == 0 => Ok(()),
        Ok(output) => Err(format!(
            "fast-forward to the fetched upstream failed with exit status {}",
            output.status
        )),
        Err(error) => Err(bounded(&error.message)),
    }
}
