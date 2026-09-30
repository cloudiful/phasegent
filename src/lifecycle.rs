//! Local lifecycle side effects for bootstrap and provider-backed issue
//! create/close.
//!
//! Every helper here is best-effort: local Git state must never turn a
//! successful remote operation into a failure. Outcomes are structured so
//! callers can surface bounded warnings on stderr while keeping stdout JSON
//! compatible. Repository identity is compared as OWNER/REPOSITORY only;
//! remote URLs and credentials are never echoed.

use crate::git_runner::GitRunner;
use crate::hooks::{self, InstallOutcome};
use crate::remote;
use std::path::{Path, PathBuf};

#[path = "lifecycle/issue_switch.rs"]
mod issue_switch;
// `CreateSwitchOutcome` is named by phase3 tests and P5 flows; the
// production create arm only calls `.warning()` on the value.
#[allow(unused_imports)]
pub use issue_switch::{
    CreateSwitchOutcome, ExplicitLinkOutcome, ExplicitLinkParams, IssueSwitchParams,
    create_link_and_switch, link_explicit_branch,
};

pub const MAX_WARNING_CHARS: usize = 200;

/// Resolves the OWNER/REPOSITORY identity of the current checkout's origin.
/// `None` covers "not a git checkout", "no configured origin", and
/// unparseable remotes; callers treat all three as "no matching local
/// repository" without failing. The URL itself is never returned.
pub fn origin_identity(runner: &dyn GitRunner) -> Option<String> {
    let output = runner.run(&["remote", "get-url", "origin"]).ok()?;
    if output.status != 0 {
        return None;
    }
    remote::parse_remote(output.stdout.trim())
        .ok()
        .map(|parsed| parsed.repository)
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_WARNING_CHARS).collect()
}

pub fn current_checkout_matches(
    runner: &dyn GitRunner,
    explicit_repository: Option<&str>,
) -> Result<(), String> {
    let Some(origin) = origin_identity(runner) else {
        return Err("current directory has no git origin; skipping branch binding".to_owned());
    };
    if let Some(explicit) = explicit_repository
        && origin != explicit
    {
        return Err(format!(
            "git origin '{origin}' does not match explicit repository '{explicit}'; \
             skipping branch binding"
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub enum HookAutoInstall {
    Installed(InstallOutcome),
    Skipped {
        reason: String,
    },
    /// Matching checkout but installation failed locally; bootstrap stays
    /// successful and the bounded reason becomes a warning.
    Failed {
        reason: String,
    },
}

impl HookAutoInstall {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Installed(outcome) => serde_json::json!({
                "installed": outcome.installed,
                "updated": outcome.updated,
                "warnings": outcome.warnings,
            }),
            Self::Skipped { reason } | Self::Failed { reason } => {
                serde_json::json!({ "skipped": true, "reason": bounded(reason) })
            }
        }
    }

    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Failed { reason } => Some(bounded(reason)),
            _ => None,
        }
    }
}

/// Installs managed hooks when (and only when) the current checkout's origin
/// identifies exactly `bootstrap_repository`. No-origin checkouts, non-Git
/// directories, and mismatches skip silently with a structured reason; a
/// failed install degrades to `Failed` instead of failing the caller.
pub fn auto_install_hooks(
    runner: &dyn GitRunner,
    working_dir: &Path,
    bootstrap_repository: &str,
) -> HookAutoInstall {
    let Some(origin) = origin_identity(runner) else {
        return HookAutoInstall::Skipped {
            reason: "current directory has no git origin; managed hooks not installed".to_owned(),
        };
    };
    if origin != bootstrap_repository {
        return HookAutoInstall::Skipped {
            reason: format!(
                "git origin '{origin}' does not match bootstrap repository \
                 '{bootstrap_repository}'; managed hooks not installed"
            ),
        };
    }
    match hooks::install_in(runner, working_dir) {
        Ok(outcome) => HookAutoInstall::Installed(outcome),
        Err(error) => HookAutoInstall::Failed {
            reason: format!("managed hook installation failed: {}", error.message),
        },
    }
}

pub fn branch_prefix_for_tracker(tracker: Option<&str>) -> &'static str {
    match tracker.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("bug") => "fix",
        _ => "feat",
    }
}

pub fn branch_name_for_issue(tracker: Option<&str>, issue_id: u64) -> String {
    format!("{}/{issue_id}", branch_prefix_for_tracker(tracker))
}

#[derive(Debug, PartialEq, Eq)]
pub enum AutoReleaseLeaseOutcome {
    Released {
        released: u64,
    },
    /// The current repository identity or the lease store could not be
    /// reached. The remote close already succeeded, so this is a
    /// bounded warning only.
    Warning {
        reason: String,
    },
}

impl AutoReleaseLeaseOutcome {
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Warning { reason } => Some(bounded(reason)),
            Self::Released { .. } => None,
        }
    }
}

/// Release every active worktree lease that the current repository
/// holds for a closed issue, across every session, after a successful
/// remote close (issue 305 Task 3, widened by issue 537 Phase 2, made
/// session-independent by issue 575 Phase 1).
///
/// Closing an issue converges its lifecycle, so the release never needs
/// a session identity: every `active` lease of `(repo, issue)` flips to
/// `retained`, whether or not the closer owns one. An explicit
/// `--worktree-session` or an environment `PHASEGENT_SESSION_ID` is
/// optional and only attributes the `release_reason`
/// (`issue closed: <session>`); without one the reason is the plain
/// `issue closed`, so no owner is guessed. The canonical repository
/// identity is resolved here and the atomic flip is delegated to
/// [`crate::worktree::release_active_leases_for_issue`], so a different
/// issue or repository is never touched. Every failure degrades to a
/// bounded warning because the remote close has already succeeded.
pub fn release_closed_issue_leases(
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
) -> AutoReleaseLeaseOutcome {
    let identity = match crate::worktree::repo_identity(runner, repo_path) {
        Ok(identity) => identity,
        Err(error) => {
            return AutoReleaseLeaseOutcome::Warning {
                reason: format!(
                    "issue {issue} closed; could not resolve repository identity for \
                     lease release: {}",
                    bounded(&error.message)
                ),
            };
        }
    };
    let reason = match session.map(str::trim).filter(|value| !value.is_empty()) {
        Some(session) => format!("issue closed: {session}"),
        None => "issue closed".to_owned(),
    };
    match crate::worktree::release_active_leases_for_issue(&identity, issue, &reason) {
        Ok(released) => AutoReleaseLeaseOutcome::Released { released },
        Err(error) => AutoReleaseLeaseOutcome::Warning {
            reason: format!(
                "issue {issue} closed; worktree lease release failed: {}",
                bounded(&error.message)
            ),
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AutoCleanupOutcome {
    Noop,
    Cleaned {
        removed: u64,
        kept: Vec<String>,
    },
    /// The repository identity or the lease store could not be resolved.
    /// Nothing was deleted; the remote close already succeeded, so this
    /// is a bounded warning only.
    Warning {
        reason: String,
    },
}

impl AutoCleanupOutcome {
    /// Stderr warning lines: one per preserved directory (reason first,
    /// directory verbatim), or the single warning of the
    /// unresolvable-context arm. Empty for a no-op and for a cleanup
    /// that removed every candidate.
    pub fn warnings(&self) -> Vec<String> {
        match self {
            Self::Noop => Vec::new(),
            Self::Cleaned { kept, .. } => kept.clone(),
            Self::Warning { reason } => vec![bounded(reason)],
        }
    }
}

/// Canonicalise `path`, falling back to the path itself when it no
/// longer exists so a just-removed directory still compares equal to
/// its recorded lease path.
fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn same_directory(left: &Path, right: &Path) -> bool {
    canonical_or_self(left) == canonical_or_self(right)
}

/// True when `directory` is the repository's main working tree: the
/// checkout whose per-worktree Git dir *is* the shared common dir.
/// Every linked worktree resolves `git rev-parse --git-dir` to
/// `<common>/worktrees/<name>` instead, so it never matches. A probe
/// that cannot answer is an `Err`; callers must then keep the
/// directory, because the guard cannot be verified.
fn is_main_checkout(
    runner: &dyn crate::worktree::WorktreeRunner,
    directory: &Path,
    identity: &str,
) -> Result<bool, crate::worktree::WorktreeError> {
    let output = runner.run(&["rev-parse", "--git-dir"], directory)?;
    if output.status != 0 {
        return Err(crate::worktree::WorktreeError::new(
            "git",
            format!(
                "git rev-parse --git-dir failed with exit status {}",
                output.status
            ),
        ));
    }
    let raw = output.stdout.trim();
    if raw.is_empty() {
        return Err(crate::worktree::WorktreeError::new(
            "git",
            "git rev-parse --git-dir returned an empty path",
        ));
    }
    let resolved = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        directory.join(raw)
    };
    Ok(canonical_or_self(&resolved) == canonical_or_self(Path::new(identity)))
}

/// Delete the closed issue's clean worktree directories in this
/// repository, after the remote close and the lease release succeeded.
///
/// The dirty probe ([`crate::worktree::is_clean`]) and the removal call
/// ([`crate::worktree::worktree_remove`]) are the same primitives the
/// `worktree prune --remove` pass is built on; only the guards differ,
/// because a close converges the issue's lifecycle immediately instead
/// of waiting for the stale window. A directory is removed only when
/// all three guards hold:
///
/// 1. the directory is clean — `git status --porcelain` is empty, so
///    untracked files count as dirty;
/// 2. no *other* session holds an `active` lease pointing at the
///    directory. The close chain flips this issue's `active` leases to
///    `retained` before this helper runs (issue 575 Phase 1), so an
///    `active` row seen here belongs to another issue, or to another
///    session of this repository; the closing session's own row is
///    exempted for the callers that reach the guard without a preceding
///    flip, such as the `issue sync` report mode. `session == None`
///    makes every still-active row foreign, which keeps an
///    unconverged directory instead of deleting it;
/// 3. the directory is not the repository's main working tree, i.e.
///    its `git rev-parse --git-dir` is not the shared common dir.
///
/// Every blocked, failed, or unreadable candidate is kept and returned
/// as one reason-first warning entry naming the directory verbatim, so
/// the operator can act on it; only embedded error text is bounded.
/// Lease rows are never touched here (the close chain flipped them to
/// `retained` before this helper runs, and they stay as audit records)
/// and branches are never deleted. Every failure degrades to a warning
/// because the remote close has already succeeded.
pub fn cleanup_closed_issue_worktrees(
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
) -> AutoCleanupOutcome {
    if issue == 0 {
        return AutoCleanupOutcome::Noop;
    }
    let session = session.map(str::trim).filter(|value| !value.is_empty());
    let identity = match crate::worktree::repo_identity(runner, repo_path) {
        Ok(identity) => identity,
        Err(error) => {
            return AutoCleanupOutcome::Warning {
                reason: format!(
                    "issue {issue} closed; could not resolve repository identity for \
                     worktree cleanup: {}",
                    bounded(&error.message)
                ),
            };
        }
    };
    let storage = match crate::infra::storage::Storage::open() {
        Ok(storage) => storage,
        Err(error) => {
            return AutoCleanupOutcome::Warning {
                reason: format!(
                    "issue {issue} closed; could not open the lease store for worktree \
                     cleanup: {}",
                    bounded(&error)
                ),
            };
        }
    };
    if let Err(error) = crate::worktree::leases::ensure_schema(&storage) {
        return AutoCleanupOutcome::Warning {
            reason: format!(
                "issue {issue} closed; could not initialise the lease store for worktree \
                 cleanup: {}",
                bounded(&error)
            ),
        };
    }
    let rows = match crate::worktree::leases::list_for_repo(&storage, &identity) {
        Ok(rows) => rows,
        Err(error) => {
            return AutoCleanupOutcome::Warning {
                reason: format!(
                    "issue {issue} closed; could not read worktree leases for cleanup: {}",
                    bounded(&error.message)
                ),
            };
        }
    };
    if !rows.iter().any(|row| row.issue == issue) {
        return AutoCleanupOutcome::Noop;
    }
    let mut removed = 0u64;
    let mut kept: Vec<String> = Vec::new();
    for row in rows.iter().filter(|row| row.issue == issue) {
        let directory = PathBuf::from(&row.worktree_path);
        if !directory.exists() {
            continue;
        }
        match is_main_checkout(runner, &directory, &identity) {
            Ok(true) => {
                kept.push(format!(
                    "issue {issue} closed; kept worktree (main checkout is never removed): {}",
                    row.worktree_path
                ));
                continue;
            }
            Ok(false) => {}
            Err(error) => {
                kept.push(format!(
                    "issue {issue} closed; kept worktree (could not verify the \
                     main-checkout guard: {}): {}",
                    bounded(&error.message),
                    row.worktree_path
                ));
                continue;
            }
        }
        let foreign_active = rows.iter().find(|other| {
            other.status == crate::worktree::LEASE_STATUS_ACTIVE
                && session != Some(other.session.as_str())
                && same_directory(Path::new(&other.worktree_path), &directory)
        });
        if let Some(other) = foreign_active {
            kept.push(format!(
                "issue {issue} closed; kept worktree (session '{}' holds an active lease \
                 for issue {}): {}",
                other.session, other.issue, row.worktree_path
            ));
            continue;
        }
        match crate::worktree::is_clean(runner, &directory) {
            Ok(true) => {}
            Ok(false) => {
                kept.push(format!(
                    "issue {issue} closed; kept worktree (uncommitted or untracked files): {}",
                    row.worktree_path
                ));
                continue;
            }
            Err(error) => {
                kept.push(format!(
                    "issue {issue} closed; kept worktree (cleanliness probe failed: {}): {}",
                    bounded(&error.message),
                    row.worktree_path
                ));
                continue;
            }
        }
        // Run the removal from inside the candidate: it exists at this
        // point, while the close's own working directory may already be
        // a removed sibling by the time a later candidate is handled.
        match crate::worktree::worktree_remove(runner, &directory, &directory) {
            Ok(()) => removed += 1,
            Err(error) => kept.push(format!(
                "issue {issue} closed; worktree removal failed ({}): {}",
                bounded(&error.message),
                row.worktree_path
            )),
        }
    }
    if removed == 0 && kept.is_empty() {
        return AutoCleanupOutcome::Noop;
    }
    AutoCleanupOutcome::Cleaned { removed, kept }
}
