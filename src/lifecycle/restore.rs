//! Capture and restore the pre-issue primary-checkout baseline.
//!
//! Issue create writes a pending baseline immediately before its safe
//! automatic checkout. Issue close, after the remote close, lease release and
//! guarded worktree cleanup succeeded, may return the verified main checkout
//! to that branch and fast-forward only that branch's own configured upstream.
//! Every step is best-effort and conservative: missing evidence, dirty state,
//! another active lease, another hosted session or a third-party branch change
//! skips the mutation with a bounded warning and never fails the close.

use std::path::{Path, PathBuf};

use crate::branch_links::{read_origin_url, resolve_repo_key};
use crate::git_runner::GitRunner;
use crate::infra::storage::Storage;
use crate::worktree::opencode::{OpenCodeApi, occupants_of_directory};
use crate::worktree::{WorktreeRunner, bounded};

#[path = "restore/git.rs"]
mod git;
#[path = "restore/store.rs"]
pub(crate) mod store;

#[cfg(test)]
#[path = "restore/restore_tests.rs"]
mod restore_tests;
#[cfg(test)]
#[path = "restore/test_support.rs"]
pub(crate) mod test_support;
#[cfg(test)]
#[path = "restore/tests.rs"]
mod tests;

#[allow(unused_imports)]
pub(crate) use store::{
    BaselineRecord, BaselineScope, CLEAN_EVIDENCE, CheckoutState, InsertOutcome, NewBaseline,
    UpdateState, ensure_schema, insert_pending, mark_restored, read_baseline, set_update_state,
    transition_pending,
};

/// Inputs for [`prepare_pending`], bundled to stay under the argument-count
/// lint. Every value is already established by the caller's switch gates.
pub(crate) struct CaptureParams<'a> {
    pub storage: &'a Storage,
    pub worktree: &'a dyn WorktreeRunner,
    pub repo_path: &'a Path,
    pub repo_key: &'a str,
    pub provider: &'a str,
    pub project: &'a str,
    pub issue_number: u64,
    pub issue_branch: &'a str,
    pub baseline_branch: &'a str,
    pub default_branch: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CaptureOutcome {
    /// The checkout is not a verified primary checkout, so there is no
    /// baseline to capture; the pre-existing switch decision is unchanged.
    Skipped { reason: String },
    /// The pending baseline is durable and the caller may switch.
    Pending { scope: BaselineScope },
    /// No durable evidence could be written, or the baseline inputs are
    /// unusable; the caller must leave the checkout untouched.
    Failed { reason: String },
}

/// Persist a pending baseline for an eligible automatic primary-checkout
/// switch. The caller invokes this immediately before its checkout command.
pub(crate) fn prepare_pending(params: &CaptureParams<'_>) -> CaptureOutcome {
    match crate::worktree::checkout_git_dirs(params.worktree, params.repo_path) {
        Ok((git_dir, common_dir)) if !git_dir.is_empty() && git_dir == common_dir => {}
        Ok(_) => {
            return CaptureOutcome::Skipped {
                reason: "checkout is not the primary checkout".to_owned(),
            };
        }
        Err(error) => {
            return CaptureOutcome::Skipped {
                reason: format!(
                    "could not verify the primary checkout: {}",
                    crate::lifecycle::bounded(&error.message)
                ),
            };
        }
    }
    let actual_branch =
        match crate::worktree::git::current_branch_for(params.worktree, params.repo_path) {
            Ok(branch) => branch,
            Err(error) => {
                return CaptureOutcome::Failed {
                    reason: format!(
                        "could not re-read the current branch: {}",
                        crate::lifecycle::bounded(&error.message)
                    ),
                };
            }
        };
    if actual_branch != params.baseline_branch {
        return CaptureOutcome::Failed {
            reason: format!(
                "current branch changed to '{actual_branch}' after the switch gates; \
                 refusing to capture a stale baseline"
            ),
        };
    }
    let baseline_head = match crate::worktree::head_rev(params.worktree, params.repo_path) {
        Ok(Some(head)) => head,
        Ok(None) => {
            return CaptureOutcome::Failed {
                reason: "HEAD is unborn or cannot be resolved".to_owned(),
            };
        }
        Err(error) => {
            return CaptureOutcome::Failed {
                reason: format!(
                    "could not resolve HEAD: {}",
                    crate::lifecycle::bounded(&error.message)
                ),
            };
        }
    };
    let checkout_path = canonical_checkout_path(params.repo_path);
    let scope = match BaselineScope::new(
        params.repo_key,
        params.provider,
        params.project,
        params.issue_number,
        &checkout_path,
    ) {
        Ok(scope) => scope,
        Err(reason) => return CaptureOutcome::Failed { reason },
    };
    if let Err(error) = ensure_schema(&params.storage.connection) {
        return CaptureOutcome::Failed { reason: error };
    }
    let now = crate::worktree::now_unix_secs().max(1);
    let new = NewBaseline {
        scope: &scope,
        issue_branch: params.issue_branch,
        baseline_branch: params.baseline_branch,
        baseline_head: &baseline_head,
        clean_evidence: CLEAN_EVIDENCE,
        default_branch: params.default_branch,
        recorded_at: now,
    };
    match insert_pending(&params.storage.connection, &new) {
        Ok(_) => CaptureOutcome::Pending { scope },
        Err(reason) => CaptureOutcome::Failed { reason },
    }
}

/// Classify the branch actually left behind by the checkout command.
///
/// `active` requires the captured issue branch; `aborted` requires both the
/// original branch and commit to be unchanged; every other or unreadable
/// result stays `pending` so close-time recovery can make the final call
/// without a guessed rollback.
pub(crate) fn finalize_after_checkout(
    worktree: &dyn WorktreeRunner,
    storage: &Storage,
    scope: &BaselineScope,
) -> Result<CheckoutState, String> {
    let record = read_baseline(&storage.connection, scope)?
        .ok_or_else(|| "checkout baseline is missing after capture".to_owned())?;
    let checkout_path = Path::new(&record.scope.checkout_path);
    let branch = crate::worktree::git::current_branch_for(worktree, checkout_path).ok();
    let head = crate::worktree::head_rev(worktree, checkout_path)
        .ok()
        .flatten();
    match branch.as_deref() {
        Some(branch) if branch == record.issue_branch => {
            transition_pending(&storage.connection, scope, CheckoutState::Active)?;
            Ok(CheckoutState::Active)
        }
        Some(branch)
            if branch == record.baseline_branch
                && head.as_deref() == Some(record.baseline_head.as_str()) =>
        {
            transition_pending(&storage.connection, scope, CheckoutState::Aborted)?;
            Ok(CheckoutState::Aborted)
        }
        _ => Ok(CheckoutState::Pending),
    }
}

/// A verified main checkout and its repository identity, resolved before the
/// close's worktree cleanup so a removed working directory is never needed
/// again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CloseTarget {
    pub identity: String,
    pub main_checkout: PathBuf,
}

/// Resolve the repository's verified main checkout without mutating anything.
/// Resolution failures keep their bounded reason so a recorded baseline can
/// turn them into a visible warning rather than a silent no-op.
pub(crate) fn resolve_close_target(
    worktree: &dyn WorktreeRunner,
    repo_path: &Path,
) -> Result<CloseTarget, String> {
    let identity = crate::worktree::repo_identity(worktree, repo_path).map_err(|error| {
        format!(
            "could not resolve repository identity: {}",
            bounded(&error.message)
        )
    })?;
    let resolver = crate::lifecycle::cleanup::TargetResolver::new(repo_path, &identity);
    let main_checkout = resolver
        .get()
        .resolve()
        .map_err(|reason| format!("no verified main checkout: {reason}"))?
        .to_path_buf();
    let main_checkout = std::fs::canonicalize(&main_checkout).unwrap_or(main_checkout);
    Ok(CloseTarget {
        identity,
        main_checkout,
    })
}

/// Resolve the current checkout's repository key and whether it is the
/// originless local fallback, without touching the main checkout.
fn checkout_repo_key(repo_path: &Path) -> Option<(String, bool)> {
    let git = crate::git_runner::ProcessGitRunner::in_directory(repo_path);
    let origin = read_origin_url(&git);
    let root = crate::branch_links::checkout_root(&git);
    resolve_repo_key(origin.as_deref(), &root)
        .ok()
        .map(|repo| (repo.key, repo.local_only))
}

/// A visible warning for an unresolvable close preparation only when a
/// recorded baseline exists for the known repository and provider/project
/// scope. An absent baseline, an unknown scope, or an unreadable store stays
/// a quiet no-op.
///
/// An origin-backed checkout matches the recorded key directly. An originless
/// linked caller has a different `local:` key than the primary checkout it
/// came from, so its candidates are correlated through the safely verified
/// shared Git common-directory identity; a candidate in another repository is
/// never accepted as evidence.
pub(crate) fn close_restore_unavailable_warning(
    storage: &Storage,
    repo_path: &Path,
    provider: Option<&str>,
    project: Option<&str>,
    issue_number: u64,
    reason: &str,
) -> Option<String> {
    let provider = provider.map(str::trim).filter(|value| !value.is_empty())?;
    let project = project.map(str::trim).filter(|value| !value.is_empty())?;
    let (repo_key, local_only) = checkout_repo_key(repo_path)?;
    let has_baseline = if local_only {
        let identity = crate::worktree::repo_identity(
            &crate::worktree::ProcessWorktreeRunner::new(),
            repo_path,
        )
        .ok()?;
        let candidates = store::baseline_candidates_for_scope(
            &storage.connection,
            provider,
            project,
            issue_number,
        )
        .ok()?;
        candidates.iter().any(|(candidate_key, checkout_path)| {
            candidate_key.starts_with(crate::branch_links::LOCAL_KEY_PREFIX)
                && crate::worktree::repo_identity(
                    &crate::worktree::ProcessWorktreeRunner::new(),
                    Path::new(checkout_path),
                )
                .is_ok_and(|candidate_identity| candidate_identity == identity)
        })
    } else {
        store::baseline_exists_for_issue(
            &storage.connection,
            &repo_key,
            provider,
            project,
            issue_number,
        )
        .ok()?
    };
    has_baseline.then(|| {
        format!(
            "issue {issue_number} closed; checkout restoration was skipped: {}",
            bounded(reason)
        )
    })
}

/// Why a close's baseline restore must be skipped because the closed number
/// is ambiguous among the durable tasks of this provider, project and number
/// in the verified repository.
///
/// `checkout_baselines` carries no instance or task id, so a recorded baseline
/// can only be attributed when exactly one durable task in this repository
/// matches the provider, project and number. More than one matching task — two
/// canonical instances of the same number — keeps the restore skipped instead
/// of restoring through a guessed baseline. A repository whose key or
/// membership cannot be proven also skips, and a number with no durable task
/// here keeps the existing no-op path.
pub(crate) fn ambiguous_restore_warning(
    storage: &Storage,
    repo_path: &Path,
    provider: Option<&str>,
    project: Option<&str>,
    issue_number: u64,
    task_id: Option<i64>,
) -> Option<String> {
    let provider = provider.map(str::trim).filter(|value| !value.is_empty())?;
    let project = project.map(str::trim).filter(|value| !value.is_empty())?;
    let Some((repo_key, _)) = checkout_repo_key(repo_path) else {
        return Some(format!(
            "issue {issue_number} closed; checkout restoration was skipped because the \
             repository identity could not be proven"
        ));
    };
    let memberships = match crate::task_identity::bindings_for_repo_number_with(
        &storage.connection,
        &repo_key,
        issue_number,
    ) {
        Ok(memberships) => memberships,
        Err(reason) => {
            return Some(format!(
                "issue {issue_number} closed; checkout restoration was skipped because the \
                 durable task membership could not be read: {}",
                bounded(&reason)
            ));
        }
    };
    let mut matching: Vec<i64> = memberships
        .iter()
        .filter(|binding| {
            let key = binding.key();
            key.provider() == provider && key.project() == project
        })
        .map(|binding| binding.task_id())
        .collect();
    matching.sort_unstable();
    matching.dedup();
    if task_id.is_some_and(|id| !matching.contains(&id)) {
        return Some(format!(
            "issue {issue_number} closed; checkout restoration was skipped because the routed \
             task membership could not be proven"
        ));
    }
    if matching.len() > 1 {
        return Some(format!(
            "issue {issue_number} closed; checkout restoration was skipped because {} durable \
             tasks in this repository share provider '{provider}', project '{project}' and \
             number {issue_number}",
            matching.len()
        ));
    }
    None
}

pub(crate) struct CloseRestoreParams<'a> {
    pub storage: &'a Storage,
    pub git: &'a dyn GitRunner,
    pub worktree: &'a dyn WorktreeRunner,
    pub api: &'a dyn OpenCodeApi,
    pub target: &'a CloseTarget,
    pub provider: Option<&'a str>,
    pub project: Option<&'a str>,
    pub issue_number: u64,
    pub session: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpstreamOutcome {
    Updated,
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestoreOutcome {
    Noop,
    Restored {
        branch: String,
        update: UpstreamOutcome,
    },
    Warning {
        reason: String,
    },
}

impl RestoreOutcome {
    pub(crate) fn warnings(self) -> Vec<String> {
        match self {
            Self::Noop => Vec::new(),
            Self::Restored {
                update: UpstreamOutcome::Updated,
                ..
            } => Vec::new(),
            Self::Restored { branch, update } => {
                let detail = match update {
                    UpstreamOutcome::Skipped(reason) | UpstreamOutcome::Failed(reason) => reason,
                    UpstreamOutcome::Updated => String::new(),
                };
                vec![format!(
                    "checkout restored to '{branch}'; upstream update was not completed: {detail}"
                )]
            }
            Self::Warning { reason } => vec![reason],
        }
    }
}

/// Restore the captured baseline after a successful close, lease release and
/// guarded worktree cleanup. The caller must invoke this only after those
/// steps; every failure here is a warning, never a close failure.
pub(crate) fn restore_on_close(params: &CloseRestoreParams<'_>) -> RestoreOutcome {
    if let Err(error) = crate::worktree::ensure_schema(params.storage) {
        return warning(
            params.issue_number,
            "could not prepare the lease store",
            &error,
        );
    }
    if let Err(error) = ensure_schema(&params.storage.connection) {
        return warning(
            params.issue_number,
            "could not prepare the checkout baseline store",
            &error,
        );
    }
    let repo_key = match resolve_repo_key(
        read_origin_url(params.git).as_deref(),
        &params.target.main_checkout,
    ) {
        Ok(repo) => repo.key,
        Err(error) => {
            return warning(
                params.issue_number,
                "could not resolve the repository key",
                &error,
            );
        }
    };
    let checkout_path = canonical_checkout_path(&params.target.main_checkout);
    let (provider, project) = match (params.provider, params.project) {
        (Some(provider), Some(project))
            if !provider.trim().is_empty() && !project.trim().is_empty() =>
        {
            (provider, project)
        }
        // An unknown scope is never permission to look at another scope.
        _ => return RestoreOutcome::Noop,
    };
    let scope = match BaselineScope::new(
        &repo_key,
        provider,
        project,
        params.issue_number,
        &checkout_path,
    ) {
        Ok(scope) => scope,
        Err(reason) => {
            return warning(
                params.issue_number,
                "could not scope the checkout baseline",
                &reason,
            );
        }
    };
    let mut record = match read_baseline(&params.storage.connection, &scope) {
        Ok(Some(record)) => record,
        Ok(None) => return RestoreOutcome::Noop,
        Err(error) => {
            return warning(
                params.issue_number,
                "could not read the checkout baseline",
                &error,
            );
        }
    };

    if record.state == CheckoutState::Pending {
        match recover_pending(params, &record) {
            Ok(Some(state)) => record.state = state,
            Ok(None) => return RestoreOutcome::Noop,
            Err(reason) => {
                return warning(
                    params.issue_number,
                    "pending checkout baseline remains unresolved",
                    &reason,
                );
            }
        }
    }
    if record.state == CheckoutState::Aborted {
        return RestoreOutcome::Noop;
    }

    if record.restored_at.is_some() {
        return match record.update_state {
            UpdateState::Updated | UpdateState::Skipped => RestoreOutcome::Noop,
            _ => match retry_upstream(params, &record) {
                Ok(outcome) => finished(record.baseline_branch.clone(), outcome),
                Err(reason) => warning(
                    params.issue_number,
                    "upstream update was not completed",
                    &reason,
                ),
            },
        };
    }

    let current = match current_branch(params) {
        Ok(branch) => branch,
        Err(reason) => {
            return warning(
                params.issue_number,
                "checkout branch could not be read before restoration",
                &reason,
            );
        }
    };
    if current == record.baseline_branch {
        // The checkout is already back on the original branch (a manual
        // switch, or a store write that failed after the checkout). Record
        // the restoration and continue with the independently retryable
        // upstream update; no branch mutation is needed.
        let now = crate::worktree::now_unix_secs().max(1);
        if let Err(error) = mark_restored(
            &params.storage.connection,
            &scope,
            UpdateState::Pending,
            now,
        ) {
            return warning(
                params.issue_number,
                "checkout is on the baseline branch but its state could not be recorded",
                &error,
            );
        }
        record.restored_at = Some(now);
        record.update_state = UpdateState::Pending;
        return match retry_upstream(params, &record) {
            Ok(outcome) => finished(record.baseline_branch.clone(), outcome),
            Err(reason) => warning(
                params.issue_number,
                "upstream update was not completed",
                &reason,
            ),
        };
    }
    if current != record.issue_branch {
        return warning(
            params.issue_number,
            "checkout is on a third-party branch",
            &format!("current branch '{current}' is neither the issue branch nor the baseline"),
        );
    }
    if let Err(reason) = main_gates(params, &record.issue_branch) {
        return warning(
            params.issue_number,
            "checkout restoration was skipped",
            &reason,
        );
    }
    match git::local_branch_exists(
        params.worktree,
        &params.target.main_checkout,
        &record.baseline_branch,
    ) {
        Ok(true) => {}
        Ok(false) => {
            return warning(
                params.issue_number,
                "checkout restoration was skipped",
                &format!(
                    "the captured baseline branch '{}' no longer exists",
                    record.baseline_branch
                ),
            );
        }
        Err(reason) => {
            return warning(
                params.issue_number,
                "checkout restoration was skipped",
                &format!(
                    "the captured baseline branch '{}' could not be verified: {reason}",
                    record.baseline_branch
                ),
            );
        }
    }
    if let Err(reason) = git::checkout(
        params.worktree,
        &params.target.main_checkout,
        &record.baseline_branch,
    ) {
        return warning(params.issue_number, "checkout restoration failed", &reason);
    }
    match current_branch(params) {
        Ok(branch) if branch == record.baseline_branch => {}
        Ok(branch) => {
            return warning(
                params.issue_number,
                "checkout restoration could not be verified",
                &format!("HEAD ended on '{branch}'"),
            );
        }
        Err(reason) => {
            return warning(
                params.issue_number,
                "checkout restoration could not be verified",
                &reason,
            );
        }
    }
    let now = crate::worktree::now_unix_secs().max(1);
    if let Err(error) = mark_restored(
        &params.storage.connection,
        &scope,
        UpdateState::Pending,
        now,
    ) {
        return warning(
            params.issue_number,
            "checkout was restored but its state could not be recorded",
            &error,
        );
    }
    record.restored_at = Some(now);
    record.update_state = UpdateState::Pending;
    match retry_upstream(params, &record) {
        Ok(outcome) => finished(record.baseline_branch.clone(), outcome),
        Err(reason) => warning(
            params.issue_number,
            "upstream update was not completed",
            &reason,
        ),
    }
}

fn recover_pending(
    params: &CloseRestoreParams<'_>,
    record: &BaselineRecord,
) -> Result<Option<CheckoutState>, String> {
    let branch = current_branch(params)?;
    if branch == record.issue_branch {
        transition_pending(
            &params.storage.connection,
            &record.scope,
            CheckoutState::Active,
        )?;
        return Ok(Some(CheckoutState::Active));
    }
    let head = head_rev(params)?;
    if branch == record.baseline_branch && head.as_deref() == Some(record.baseline_head.as_str()) {
        transition_pending(
            &params.storage.connection,
            &record.scope,
            CheckoutState::Aborted,
        )?;
        return Ok(Some(CheckoutState::Aborted));
    }
    Err(format!(
        "pending baseline does not match the captured issue branch or unchanged baseline \
         (current '{branch}')"
    ))
}

fn retry_upstream(
    params: &CloseRestoreParams<'_>,
    record: &BaselineRecord,
) -> Result<UpstreamOutcome, String> {
    let upstream = match git::resolve_upstream(
        params.worktree,
        &params.target.main_checkout,
        &record.baseline_branch,
    ) {
        Ok(upstream) => upstream,
        Err(reason) => {
            set_update_state(
                &params.storage.connection,
                &record.scope,
                UpdateState::Failed,
            )?;
            return Ok(UpstreamOutcome::Failed(reason));
        }
    };
    let git::Upstream::Configured {
        remote,
        merge_ref,
        branch,
    } = upstream
    else {
        let reason = match upstream {
            git::Upstream::None => "branch has no configured remote upstream".to_owned(),
            git::Upstream::Unsupported(reason) => reason,
            git::Upstream::Configured { .. } => unreachable!(),
        };
        set_update_state(
            &params.storage.connection,
            &record.scope,
            UpdateState::Skipped,
        )?;
        return Ok(UpstreamOutcome::Skipped(reason));
    };
    if branch.trim().is_empty() {
        set_update_state(
            &params.storage.connection,
            &record.scope,
            UpdateState::Skipped,
        )?;
        return Ok(UpstreamOutcome::Skipped(
            "configured upstream branch is empty".to_owned(),
        ));
    }
    if let Err(reason) = main_gates(params, &record.baseline_branch) {
        set_update_state(
            &params.storage.connection,
            &record.scope,
            UpdateState::Failed,
        )?;
        return Ok(UpstreamOutcome::Failed(reason));
    }
    let fetched = match git::fetch_upstream(
        params.worktree,
        &params.target.main_checkout,
        &remote,
        &merge_ref,
    ) {
        Ok(sha) => sha,
        Err(reason) => {
            set_update_state(
                &params.storage.connection,
                &record.scope,
                UpdateState::Failed,
            )?;
            return Ok(UpstreamOutcome::Failed(reason));
        }
    };
    let head = match head_rev(params) {
        Ok(Some(head)) => head,
        Ok(None) => {
            set_update_state(
                &params.storage.connection,
                &record.scope,
                UpdateState::Failed,
            )?;
            return Ok(UpstreamOutcome::Failed(
                "HEAD is unborn or cannot be resolved".to_owned(),
            ));
        }
        Err(reason) => {
            set_update_state(
                &params.storage.connection,
                &record.scope,
                UpdateState::Failed,
            )?;
            return Ok(UpstreamOutcome::Failed(reason));
        }
    };
    match git::is_ancestor(
        params.worktree,
        &params.target.main_checkout,
        &head,
        &fetched,
    ) {
        Ok(true) => {}
        Ok(false) => {
            set_update_state(
                &params.storage.connection,
                &record.scope,
                UpdateState::Failed,
            )?;
            return Ok(UpstreamOutcome::Failed(
                "local branch is ahead of or diverged from its upstream; nothing was overwritten"
                    .to_owned(),
            ));
        }
        Err(reason) => {
            set_update_state(
                &params.storage.connection,
                &record.scope,
                UpdateState::Failed,
            )?;
            return Ok(UpstreamOutcome::Failed(reason));
        }
    }
    if let Err(reason) = main_gates(params, &record.baseline_branch) {
        set_update_state(
            &params.storage.connection,
            &record.scope,
            UpdateState::Failed,
        )?;
        return Ok(UpstreamOutcome::Failed(reason));
    }
    if let Err(reason) = git::fast_forward(params.worktree, &params.target.main_checkout, &fetched)
    {
        set_update_state(
            &params.storage.connection,
            &record.scope,
            UpdateState::Failed,
        )?;
        return Ok(UpstreamOutcome::Failed(reason));
    }
    match head_rev(params) {
        Ok(Some(head)) if head == fetched => {}
        Ok(Some(head)) => {
            set_update_state(
                &params.storage.connection,
                &record.scope,
                UpdateState::Failed,
            )?;
            return Ok(UpstreamOutcome::Failed(format!(
                "fast-forward did not take effect (HEAD is '{head}')"
            )));
        }
        Ok(None) => {
            set_update_state(
                &params.storage.connection,
                &record.scope,
                UpdateState::Failed,
            )?;
            return Ok(UpstreamOutcome::Failed(
                "HEAD is unborn or cannot be resolved".to_owned(),
            ));
        }
        Err(reason) => {
            set_update_state(
                &params.storage.connection,
                &record.scope,
                UpdateState::Failed,
            )?;
            return Ok(UpstreamOutcome::Failed(reason));
        }
    }
    set_update_state(
        &params.storage.connection,
        &record.scope,
        UpdateState::Updated,
    )?;
    Ok(UpstreamOutcome::Updated)
}

fn main_gates(params: &CloseRestoreParams<'_>, expected_branch: &str) -> Result<(), String> {
    match crate::worktree::is_clean(params.worktree, &params.target.main_checkout) {
        Ok(true) => {}
        Ok(false) => {
            return Err("main checkout has uncommitted or untracked files".to_owned());
        }
        Err(error) => {
            return Err(format!(
                "main checkout cleanliness is unknown: {}",
                bounded(&error.message)
            ));
        }
    }
    let branch = current_branch(params)?;
    if branch != expected_branch {
        return Err(format!(
            "main checkout is on '{branch}', expected '{expected_branch}'"
        ));
    }
    let rows = crate::worktree::list_all_for_repo(params.storage, &params.target.identity)
        .map_err(|error| {
            format!(
                "could not read worktree leases: {}",
                bounded(&error.message)
            )
        })?;
    if let Some(row) = rows
        .iter()
        .find(|row| row.status == crate::worktree::LEASE_STATUS_ACTIVE)
    {
        return Err(format!(
            "active lease {} for issue {} still references '{}'",
            row.lease_id, row.issue, row.worktree_path
        ));
    }
    let session = params
        .session
        .map(str::trim)
        .filter(|session| !session.is_empty())
        .filter(|session| crate::worktree::opencode::is_opencode_session(session))
        .ok_or_else(|| {
            "close has no OpenCode session identity; occupancy cannot be verified".to_owned()
        })?;
    let occupants = occupants_of_directory(params.api, &params.target.main_checkout)
        .map_err(|error| format!("could not read complete host occupancy: {}", error.message))?;
    if let Some(other) = occupants.iter().find(|occupant| occupant.id != session) {
        return Err(format!(
            "OpenCode session '{}' is still hosted in the main checkout",
            other.id
        ));
    }
    Ok(())
}

fn current_branch(params: &CloseRestoreParams<'_>) -> Result<String, String> {
    crate::worktree::git::current_branch_for(params.worktree, &params.target.main_checkout)
        .map_err(|error| bounded(&error.message))
}

fn head_rev(params: &CloseRestoreParams<'_>) -> Result<Option<String>, String> {
    crate::worktree::head_rev(params.worktree, &params.target.main_checkout)
        .map_err(|error| bounded(&error.message))
}

fn finished(branch: String, update: UpstreamOutcome) -> RestoreOutcome {
    RestoreOutcome::Restored { branch, update }
}

fn warning(issue: u64, what: &str, detail: &str) -> RestoreOutcome {
    RestoreOutcome::Warning {
        reason: format!("issue {issue} closed; {what}: {}", bounded(detail)),
    }
}

fn canonical_checkout_path(path: &Path) -> String {
    let absolute = PathBuf::from(path);
    let absolute = if absolute.is_absolute() {
        absolute
    } else {
        std::env::current_dir()
            .map(|current| current.join(&absolute))
            .unwrap_or(absolute)
    };
    std::fs::canonicalize(&absolute)
        .unwrap_or(absolute)
        .to_string_lossy()
        .into_owned()
}
