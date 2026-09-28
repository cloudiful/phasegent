//! Canonical repository identity and default-branch detection.
//!
//! Links are keyed by the credential-free canonical `origin` URL
//! ([`crate::remote::canonical_git_url`]) so same-origin clones on this
//! host resolve the same rows. When no origin exists the association is
//! local-only: the key is a `local:`-prefixed absolute path and callers
//! must surface that the link does not travel across clones.
//!
//! The filesystem fallback here is intentionally *not* the worktree lease
//! `repo_identity` (git-common-dir); leases need checkout concurrency
//! while links need cross-clone portability.

//! P2 foundation API; production CLI wiring lands in P3.
#![allow(dead_code)]

use std::path::Path;

use crate::branch_context::GitRunner;
use crate::remote::canonical_git_url;

pub const LOCAL_KEY_PREFIX: &str = "local:";
pub const LEGACY_SOURCE: &str = "legacy-import";

/// Resolved repository key plus whether it is local-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRepo {
    pub key: String,
    pub local_only: bool,
}

/// Canonical key for one origin URL. Strips credentials, query,
/// fragment, trailing `.git`, and scheme so SSH and HTTPS forms of the
/// same repository match.
pub fn repo_key_for_origin(origin_url: &str) -> Result<String, String> {
    canonical_git_url(origin_url)
}

/// Filesystem fallback identity for checkouts without an origin.
/// Returns a `local:<absolute-path>` key; the path is absolutised but
/// never canonicalised through git-common-dir so lease rules stay
/// untouched.
pub fn filesystem_fallback_key(path: &Path) -> Result<String, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("could not resolve current directory: {error}"))?
            .join(path)
    };
    let text = absolute.to_string_lossy().trim().to_owned();
    if text.is_empty() {
        return Err("checkout path is empty".to_owned());
    }
    Ok(format!("{LOCAL_KEY_PREFIX}{text}"))
}

/// Resolve the durable key: canonical origin when present, otherwise
/// the local-only filesystem fallback.
pub fn resolve_repo_key(
    origin_url: Option<&str>,
    checkout_path: &Path,
) -> Result<ResolvedRepo, String> {
    if let Some(url) = origin_url.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(ResolvedRepo {
            key: repo_key_for_origin(url)?,
            local_only: false,
        });
    }
    Ok(ResolvedRepo {
        key: filesystem_fallback_key(checkout_path)?,
        local_only: true,
    })
}

/// Resolve the checkout root for filesystem-fallback keys from git
/// itself (`rev-parse --show-toplevel`), so the key is stable no matter
/// which subdirectory the caller runs in. Falls back to the process
/// working directory outside a checkout. Local-only; never touches the
/// network.
pub fn checkout_root(runner: &dyn GitRunner) -> std::path::PathBuf {
    if let Ok(output) = runner.run(&["rev-parse", "--show-toplevel"])
        && output.status == 0
    {
        let trimmed = output.stdout.trim().to_owned();
        if !trimmed.is_empty() {
            return std::path::PathBuf::from(trimmed);
        }
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Read the `origin` URL of the checkout behind `runner` without
/// exposing credentials; `None` means no origin (local-only).
pub fn read_origin_url(runner: &dyn GitRunner) -> Option<String> {
    let output = runner.run(&["remote", "get-url", "origin"]).ok()?;
    if output.status != 0 {
        return None;
    }
    let trimmed = output.stdout.trim().to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Detect the default branch from the local clone only, without touching
/// the network.
///
/// 1. `git symbolic-ref refs/remotes/origin/HEAD`, then
/// 2. the `HEAD branch:` line of `git remote show -n origin` (cached remote
///    info only; `-n` skips the `ls-remote` network query).
///
/// A cached `(unknown)` head maps to `None`. Returns `None` when unknown;
/// callers must not auto-switch then.
pub fn detect_default_branch(runner: &dyn GitRunner) -> Option<String> {
    if let Ok(output) = runner.run(&["symbolic-ref", "refs/remotes/origin/HEAD"])
        && output.status == 0
        && let Some(branch) = parse_symbolic_ref(&output.stdout)
    {
        return Some(branch);
    }
    if let Ok(output) = runner.run(&["remote", "show", "-n", "origin"])
        && output.status == 0
        && let Some(branch) = parse_remote_show(&output.stdout)
    {
        return Some(branch);
    }
    None
}

fn parse_symbolic_ref(stdout: &str) -> Option<String> {
    let value = stdout.trim();
    let suffix = value.strip_prefix("refs/remotes/origin/")?;
    let branch = suffix.trim();
    if branch.is_empty() || branch == "HEAD" || branch.contains(char::is_whitespace) {
        return None;
    }
    Some(branch.to_owned())
}

fn parse_remote_show(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("HEAD branch:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let branch: String = rest
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned();
            // Cached `remote show -n` reports `(unknown)` or `(not queried)`
            // when the head was never resolved; neither is a branch name.
            if branch.is_empty() || branch.starts_with('(') {
                continue;
            }
            return Some(branch);
        }
    }
    None
}

/// True when `branch` equals the detected default branch.
pub fn is_default_branch(branch: &str, default_branch: Option<&str>) -> bool {
    default_branch.is_some_and(|default| default == branch)
}

/// Reject using the detected default branch as an active issue branch.
pub fn validate_not_default_branch(
    branch: &str,
    default_branch: Option<&str>,
) -> Result<(), String> {
    if is_default_branch(branch, default_branch) {
        return Err(format!(
            "branch '{branch}' is the detected default branch and cannot be an active issue branch"
        ));
    }
    Ok(())
}

/// Conventional default-branch names. Refused as active issue branches
/// while cached remote-HEAD detection is unknown: an undetected `main`
/// is still `main`, so unknown must fail closed instead of reading as
/// unprotected.
pub const CONVENTIONAL_DEFAULT_BRANCHES: [&str; 2] = ["main", "master"];

/// True when `branch` must not become an active issue branch: either
/// the detected default, or a conventional default name while
/// detection is unknown.
pub fn is_protected_branch(branch: &str, default_branch: Option<&str>) -> bool {
    match default_branch
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(default) => branch == default,
        None => CONVENTIONAL_DEFAULT_BRANCHES.contains(&branch),
    }
}

/// Reject using a protected branch as an active issue branch. The
/// detected-default message matches [`validate_not_default_branch`];
/// the unknown-detection message names the conventional name and the
/// missing cache instead of guessing.
pub fn validate_not_protected_branch(
    branch: &str,
    default_branch: Option<&str>,
) -> Result<(), String> {
    if !is_protected_branch(branch, default_branch) {
        return Ok(());
    }
    if default_branch
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return Err(format!(
            "branch '{branch}' is the detected default branch and cannot be an active issue branch"
        ));
    }
    Err(format!(
        "branch '{branch}' looks like a conventional default branch and the cached remote \
         HEAD is unknown; refusing to use it as an active issue branch"
    ))
}
