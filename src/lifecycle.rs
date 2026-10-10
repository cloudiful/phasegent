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
use std::path::Path;

#[path = "lifecycle/cleanup.rs"]
pub(crate) mod cleanup;
// The close chain and the sync engine name these through the `lifecycle`
// module rather than `lifecycle::cleanup`.
#[allow(unused_imports)]
pub use cleanup::AutoCleanupOutcome;
#[allow(unused_imports)]
pub(crate) use cleanup::{
    CleanupMode, cleanup_closed_issue_worktrees, cleanup_closed_issue_worktrees_with,
};
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
