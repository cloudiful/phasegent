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
use std::path::{Path, PathBuf};

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

/// Prefix for auto-generated `<type>/<id>` branch names. `Bug` (any case)
/// maps to `fix`; every other tracker (including `Feature`, numeric ids,
/// and `None`) maps to `feat` so a missing tracker still yields `feat/<id>`.
pub fn branch_prefix_for_tracker(tracker: Option<&str>) -> &'static str {
    match tracker.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("bug") => "fix",
        _ => "feat",
    }
}

/// Auto-generate `<type>/<id>` for bare `--branch` (e.g. `feat/452`).
pub fn branch_name_for_issue(tracker: Option<&str>, issue_id: u64) -> String {
    format!("{}/{issue_id}", branch_prefix_for_tracker(tracker))
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExplicitBranchOutcome {
    /// Branch was created from `base` and bound to the new issue.
    CreatedAndBound {
        branch: String,
        base: String,
        issue_id: u64,
    },
    /// Branch already existed and is now bound to the new issue.
    ExistedAndBound { branch: String, issue_id: u64 },
    /// Target branch already binds exactly this issue; nothing changed.
    Idempotent { branch: String, issue_id: u64 },
    /// Deliberately skipped (non-Git checkout, repository mismatch);
    /// stays silent like the legacy auto-bind skip.
    Skipped { reason: String },
    /// Local failure; the remote create still succeeded.
    Warning { reason: String },
}

impl ExplicitBranchOutcome {
    /// Only genuine local failures warn; deliberate skips stay silent.
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Warning { reason } => Some(bounded(reason)),
            _ => None,
        }
    }
}

fn bind_target_branch(
    runner: &dyn GitRunner,
    branch: &str,
    issue_id: u64,
) -> ExplicitBranchOutcome {
    match branch_context::read_issue_id(runner, branch) {
        Err(error) => ExplicitBranchOutcome::Warning {
            reason: format!("local bind failed: {}", bounded(&error.message)),
        },
        Ok(Some(existing)) if existing == issue_id => ExplicitBranchOutcome::Idempotent {
            branch: branch.to_owned(),
            issue_id,
        },
        Ok(Some(existing)) => ExplicitBranchOutcome::Warning {
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
                &branch_context::config_key(branch),
                &issue_id.to_string(),
            ]);
            match output {
                Ok(result) if result.status == 0 => ExplicitBranchOutcome::ExistedAndBound {
                    branch: branch.to_owned(),
                    issue_id,
                },
                Ok(result) => ExplicitBranchOutcome::Warning {
                    reason: format!(
                        "issue {issue_id} created; git config write failed with exit \
                         status {}",
                        result.status
                    ),
                },
                Err(error) => ExplicitBranchOutcome::Warning {
                    reason: format!("local bind failed: {}", bounded(&error.message)),
                },
            }
        }
    }
}

/// Create `branch` from `base` when missing, then bind the target branch
/// (not the current checkout) to `issue_id`.
///
/// Explicit `--branch` only: the caller resolves the branch name (bare
/// `--branch` via [`branch_name_for_issue`], named via the CLI value) and
/// passes `base` (`None` defaults to `HEAD`). An existing branch is reused
/// without moving it; an existing different binding is never overwritten.
/// Every failure degrades to [`ExplicitBranchOutcome::Warning`] (or silent
/// [`ExplicitBranchOutcome::Skipped`]) so the remote create keeps its
/// success result and stdout JSON stays byte-identical.
pub fn ensure_branch_and_bind(
    runner: &dyn GitRunner,
    issue_id: u64,
    branch: &str,
    base: Option<&str>,
    explicit_repository: Option<&str>,
) -> ExplicitBranchOutcome {
    if let Err(reason) = current_checkout_matches(runner, explicit_repository) {
        return ExplicitBranchOutcome::Skipped { reason };
    }
    let branch = branch.trim();
    if branch.is_empty() {
        return ExplicitBranchOutcome::Warning {
            reason: format!("issue {issue_id} created; empty branch name, skipping branch binding"),
        };
    }
    let base = base
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("HEAD");
    let branch_ref = format!("refs/heads/{branch}");
    match runner.run(&["show-ref", "--verify", "--quiet", &branch_ref]) {
        Ok(result) if result.status == 0 => return bind_target_branch(runner, branch, issue_id),
        Ok(result) if result.status != 1 => {
            return ExplicitBranchOutcome::Warning {
                reason: format!(
                    "issue {issue_id} created; could not check branch '{branch}' \
                     (exit status {}); run 'phasegent issue bind {issue_id}' by hand",
                    result.status
                ),
            };
        }
        Ok(_) => {}
        Err(error) => {
            return ExplicitBranchOutcome::Warning {
                reason: format!("local branch check failed: {}", bounded(&error.message)),
            };
        }
    }
    match runner.run(&["branch", branch, base]) {
        Ok(result) if result.status == 0 => match bind_target_branch(runner, branch, issue_id) {
            ExplicitBranchOutcome::ExistedAndBound { branch, issue_id } => {
                ExplicitBranchOutcome::CreatedAndBound {
                    branch,
                    base: base.to_owned(),
                    issue_id,
                }
            }
            other => other,
        },
        Ok(result) => ExplicitBranchOutcome::Warning {
            reason: format!(
                "issue {issue_id} created; git branch '{branch}' from '{base}' failed \
                 with exit status {}; run 'phasegent issue bind {issue_id}' by hand",
                result.status
            ),
        },
        Err(error) => ExplicitBranchOutcome::Warning {
            reason: format!("local branch creation failed: {}", bounded(&error.message)),
        },
    }
}

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

/// Outcome of the issue-close worktree-lease release hook.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoReleaseLeaseOutcome {
    /// No explicit `--worktree-session` and no `PHASEGENT_SESSION_ID`:
    /// the lease owner is unknown, so nothing is released. The operator
    /// gets a warning instead of a guessed release.
    NoSession { reason: String },
    /// `released` active lease(s) were flipped to `retained`.
    Released { released: u64 },
    /// The current repository identity or the lease store could not be
    /// reached. The remote close already succeeded, so this is a
    /// bounded warning only.
    Warning { reason: String },
}

impl AutoReleaseLeaseOutcome {
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::NoSession { reason } | Self::Warning { reason } => Some(bounded(reason)),
            Self::Released { .. } => None,
        }
    }
}

/// Release every active worktree lease that the current repository
/// holds for a closed issue, across every session, after a successful
/// remote close (issue 305 Task 3, widened by issue 537 Phase 2).
///
/// Only an explicit `--worktree-session` or an environment
/// `PHASEGENT_SESSION_ID` counts as a session; the legacy fallback is
/// passed as `None` so the helper never guesses an owner or runs
/// without an explicit close attribution. When a session is known it
/// only attributes the `release_reason`; the release itself is not
/// session-scoped, so closing an issue converges every active lease for
/// that issue. The canonical repository identity is resolved here and
/// the atomic flip is delegated to
/// [`crate::worktree::release_active_leases_for_issue`], so a different
/// issue or repository is never touched. Every failure degrades to a
/// bounded warning because the remote close has already succeeded.
pub fn release_closed_issue_leases(
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &Path,
    issue: u64,
    session: Option<&str>,
) -> AutoReleaseLeaseOutcome {
    let Some(session) = session.map(str::trim).filter(|value| !value.is_empty()) else {
        return AutoReleaseLeaseOutcome::NoSession {
            reason: format!(
                "issue {issue} closed; no --worktree-session or PHASEGENT_SESSION_ID \
                 supplied, so active worktree leases were left untouched"
            ),
        };
    };
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
    let reason = format!("issue closed: {session}");
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

/// Outcome of the `issue close` worktree-directory cleanup hook.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoCleanupOutcome {
    /// No lease row for the issue in this repository pointed at a
    /// directory, or every candidate directory was already absent:
    /// nothing was deleted and there is nothing to report.
    Noop,
    /// The pass ran: `removed` clean directories were deleted and every
    /// entry in `kept` names a directory a guard preserved, with the
    /// reason.
    Cleaned { removed: u64, kept: Vec<String> },
    /// The repository identity or the lease store could not be resolved.
    /// Nothing was deleted; the remote close already succeeded, so this
    /// is a bounded warning only.
    Warning { reason: String },
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

/// True when both paths name the same directory, resolving symlinks and
/// relative spellings first.
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
///    directory; the closing session's own lease never blocks its own
///    cleanup, while `session == None` (the legacy fallback) makes
///    every active lease foreign and therefore keeps every active
///    row's directory;
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

fn local_failure(operation: &str, error: &BranchContextError) -> AutoBindOutcome {
    AutoBindOutcome::Warning {
        reason: format!("local {operation} failed: {}", bounded(&error.message)),
    }
}
