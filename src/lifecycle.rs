//! Local lifecycle side effects for bootstrap and provider-backed issue
//! create/close.
//!
//! Every helper here is best-effort: local Git state must never turn a
//! successful remote operation into a failure. Outcomes are structured so
//! callers can surface bounded warnings on stderr while keeping stdout JSON
//! compatible. Repository identity is compared as OWNER/REPOSITORY only;
//! remote URLs and credentials are never echoed.

use crate::branch_context::{self, BranchContextError, GitRunner};
use crate::hooks::{self, InstallOutcome};
use crate::remote;
use std::path::Path;

/// Upper bound for any warning text derived from local repository state.
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

/// Gate used before any auto-bind/auto-unbind: the checkout must have an
/// origin, and an explicit repository override must match that origin.
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

// ---------------------------------------------------------------------------
// Bootstrap: managed hook installation for the matching checkout only.
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum HookAutoInstall {
    /// Hooks were installed/updated in this checkout.
    Installed(InstallOutcome),
    /// Deliberately not installed; `reason` explains why (shown in JSON).
    Skipped { reason: String },
    /// Matching checkout but installation failed locally; bootstrap stays
    /// successful and the bounded reason becomes a warning.
    Failed { reason: String },
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

// ---------------------------------------------------------------------------
// Redmine issue create: best-effort auto-bind of the new issue.
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum AutoBindOutcome {
    Bound { branch: String, issue_id: u64 },
    Idempotent { branch: String, issue_id: u64 },
    Skipped { reason: String },
    Warning { reason: String },
}

impl AutoBindOutcome {
    /// Only genuine local failures warn; deliberate skips (non-Git checkout,
    /// explicit-repository mismatch) stay silent.
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Warning { reason } => Some(bounded(reason)),
            _ => None,
        }
    }
}

/// Binds the newly created Redmine issue to the current named branch when the
/// effective repository matches the checkout. Detached HEAD, an existing
/// different binding, and local write failures degrade to warnings so the
/// successfully created remote issue keeps its success result. Re-binding the
/// same issue is idempotent. An existing different binding is never
/// overwritten (`replace` is always disabled here).
pub fn bind_created_issue(
    runner: &dyn GitRunner,
    issue_id: u64,
    explicit_repository: Option<&str>,
) -> AutoBindOutcome {
    if let Err(reason) = current_checkout_matches(runner, explicit_repository) {
        return AutoBindOutcome::Skipped { reason };
    }
    let branch = match branch_context::current_branch(runner) {
        Ok(branch) => branch,
        Err(error) if error.kind == "branch" => {
            return AutoBindOutcome::Warning {
                reason: format!(
                    "issue {issue_id} created; HEAD is detached, run \
                     'phasegent issue bind {issue_id}' on a named branch"
                ),
            };
        }
        Err(error) => return local_failure("bind", &error),
    };
    match branch_context::read_issue_id(runner, &branch) {
        Err(error) => local_failure("bind", &error),
        Ok(Some(existing)) if existing == issue_id => {
            AutoBindOutcome::Idempotent { branch, issue_id }
        }
        Ok(Some(existing)) => AutoBindOutcome::Warning {
            reason: format!(
                "issue {issue_id} created; branch '{branch}' remains bound to \
                 issue {existing}; use 'phasegent issue bind {issue_id} --replace' \
                 to switch bindings"
            ),
        },
        Ok(None) => {
            let output = runner.run(&[
                "config",
                "--local",
                &branch_context::config_key(&branch),
                &issue_id.to_string(),
            ]);
            match output {
                Ok(result) if result.status == 0 => AutoBindOutcome::Bound { branch, issue_id },
                Ok(result) => AutoBindOutcome::Warning {
                    reason: format!(
                        "issue {issue_id} created; git config write failed with exit \
                         status {}",
                        result.status
                    ),
                },
                Err(error) => local_failure("bind", &error),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Redmine issue close: best-effort unbind of the exact closed issue.
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum AutoUnbindOutcome {
    Unbound { branch: String, issue_id: u64 },
    Noop { reason: String },
    Warning { reason: String },
}

impl AutoUnbindOutcome {
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Warning { reason } => Some(bounded(reason)),
            _ => None,
        }
    }
}

/// Removes the current branch's binding only when it points at exactly the
/// closed issue. Other branches, other issues, missing bindings, and detached
/// HEAD stay untouched; a failed local unbind degrades to a warning so the
/// successful remote close stands.
pub fn unbind_closed_issue(
    runner: &dyn GitRunner,
    issue_id: u64,
    explicit_repository: Option<&str>,
) -> AutoUnbindOutcome {
    if let Err(reason) = current_checkout_matches(runner, explicit_repository) {
        return AutoUnbindOutcome::Noop { reason };
    }
    let branch = match branch_context::current_branch(runner) {
        Ok(branch) => branch,
        Err(_) => {
            // Detached or unreadable HEAD: nothing branch-scoped to unbind.
            return AutoUnbindOutcome::Noop {
                reason: "HEAD is detached or unavailable; no binding removed".to_owned(),
            };
        }
    };
    let bound = match branch_context::read_issue_id(runner, &branch) {
        Ok(bound) => bound,
        Err(error) => {
            return AutoUnbindOutcome::Warning {
                reason: format!(
                    "issue {issue_id} closed; could not read local binding: {}",
                    bounded(&error.message)
                ),
            };
        }
    };
    if bound != Some(issue_id) {
        return AutoUnbindOutcome::Noop {
            reason: format!(
                "branch '{branch}' is bound to {}, not the closed issue {issue_id}",
                bound
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "no issue".to_owned()),
            ),
        };
    }
    match runner.run(&[
        "config",
        "--local",
        "--unset",
        &branch_context::config_key(&branch),
    ]) {
        Ok(result) if result.status == 0 || result.status == 5 => {
            AutoUnbindOutcome::Unbound { branch, issue_id }
        }
        Ok(result) => AutoUnbindOutcome::Warning {
            reason: format!(
                "issue {issue_id} closed; git config unset failed with exit status {}",
                result.status
            ),
        },
        Err(error) => AutoUnbindOutcome::Warning {
            reason: format!(
                "issue {issue_id} closed; could not remove local binding: {}",
                bounded(&error.message)
            ),
        },
    }
}

fn local_failure(operation: &str, error: &BranchContextError) -> AutoBindOutcome {
    AutoBindOutcome::Warning {
        reason: format!("local {operation} failed: {}", bounded(&error.message)),
    }
}
