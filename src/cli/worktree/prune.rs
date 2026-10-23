//! Executor and execution model for `worktree prune`.
//!
//! `worktree prune` is a global cleanup whose output follows the global
//! output mode and keeps an explicit machine-readable selection:
//!
//! * Without `--repo` the pass enumerates every repository identity the local
//!   lease store records (even outside Git) and resolves each through its main
//!   checkout, skipping the ones it cannot verify with a bounded reason instead
//!   of guessing. `--repo PATH` restricts the pass to one verified checkout.
//! * Removal is the default; `--dryrun`/`--dry-run` is the read-only preview
//!   that mutates nothing.
//! * The output format follows the global `--output auto|text|json`
//!   selection: the automatic default reads the machine document on a pipe and
//!   for every agent role, and the concise English text on a human terminal.
//!   `--json` / `--format json` selects the aggregate JSON envelope and
//!   `--format text` the English text; an explicit legacy selection wins over
//!   the automatic default and is rejected when it contradicts an explicit
//!   global `--output`.
//!
//! Since issue 916 a managed worktree is removed as soon as every issue it is
//! associated with is *confirmed closed* and no `active` lease references it —
//! there is no waiting period. Age never gates directory deletion;
//! `--stale-days` survives only for the explicit `--release-stale` recovery.
//! Closure is read from the configured provider through
//! [`issue_state`](self::issue_state) (the same current-configured-provider
//! model `issue sync` uses); a preview may read closure but never mutates.
//!
//! The pass never syncs a provider, never releases a session, never
//! auto-releases an active lease, and never deletes a branch or lease row.
//! Directory removal reuses the shared *local* candidate guards (see
//! [`worktrees`]) so a main checkout, an active peer's path, or a dirty
//! directory is always kept, and adds its own running-session guard: only the
//! connected service's current foreground drains block removal, never a
//! historical session record.
//!
//! The removal stage is the only place that may move a session (issue 1061
//! P1): before it deletes a directory it returns the associated idle
//! `ses_`-leased sessions to the repository's verified main checkout and
//! confirms every move, so the directory is not removed while one of our own
//! sessions still sits in it. A running session, a foreign occupant, an
//! incomplete listing, an unresolvable associated session, an answer that names
//! another session or reports a location that cannot be read, an unavailable
//! host, a rejected, queued, or unconfirmed move, and an invalid target all
//! keep the directory. The scan stays read-only: it never enumerates history
//! and never moves a session.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::lifecycle::cleanup::TargetResolver;
use crate::output::context::OutputMode;
use crate::policy::Role;
use crate::worktree::leases::{
    ensure_schema, list_all_for_repo, recover_stale_active_leases, repo_identities,
    stale_active_leases, stale_window,
};
use crate::worktree::opencode::ProcessOpenCodeApi;
use crate::worktree::{LeaseRow, ProcessWorktreeRunner, now_unix_secs};

use super::{config_error, open_storage, storage_error};

mod active_guard;
mod host_cache;
pub(crate) mod issue_state;
mod leases;
mod worktrees;

use host_cache::ScanHostCache;
pub(crate) use issue_state::{
    ComposedIssueStateSource, IssueClosure, IssueStateFact, IssueStateReader, IssueStateSource,
    LinkScope, LinkScopeReader, configured_source,
};
pub(crate) use leases::{ReleaseStaleAction, ReleaseStaleSummary};
pub(crate) use worktrees::{
    DirectoryCandidate, PruneAction, PruneDisposition, PruneMode, PruneScope, PruneSummary,
    pinned_active_paths, prune_pass, scan_worktree_candidates,
};

use leases::{stale_recovery_summary, stale_scan_summary};

/// Report one progress notice of the prune pass. The notice updates the
/// progress ticker of a human run; a machine run never sees a progress line.
fn stderr_note(message: &str) {
    crate::output::progress::stage(&format!("worktree prune: {message}"));
}

/// Aggregate counts across every repository of one prune invocation, so the
/// text and JSON renderers report the same facts. All counts are directory-
/// based.
#[derive(Debug, Serialize)]
pub(crate) struct PruneTotals {
    pub repositories: usize,
    pub removed: usize,
    pub eligible: usize,
    pub absent: usize,
    pub kept_active: usize,
    pub kept_unknown: usize,
    pub kept_protected: usize,
    pub kept_issue: usize,
    pub failed: usize,
}

/// One repository's contribution to the aggregate. A resolved repository
/// carries its `leases` and `worktrees` reports; an unresolvable identity
/// carries only `skipped_reason`.
#[derive(Debug, Serialize)]
pub(crate) struct PruneRepoOutcome {
    pub repo_identity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leases: Option<ReleaseStaleSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<PruneSummary>,
}

/// The full `worktree prune` report. `remove` echoes the effective mode
/// (default removal, false only under `--dryrun`), so a caller can replay the
/// invocation from the recorded output.
#[derive(Debug, Serialize)]
pub(crate) struct PruneAggregate {
    pub dry_run: bool,
    pub remove: bool,
    pub release_stale: bool,
    pub stale_days: u32,
    pub repos: Vec<PruneRepoOutcome>,
    pub totals: PruneTotals,
}

/// A repository scope resolved before the pass touches anything.
enum Scope {
    Resolved { identity: String, path: PathBuf },
    Skipped { identity: String, reason: String },
}

pub(super) fn execute_prune(
    repo: Option<&str>,
    stale_days: u32,
    release_stale: bool,
    reason: Option<String>,
    dry_run: bool,
    format: &str,
    role: Role,
) -> i32 {
    let source = configured_source(role);
    let api = ProcessOpenCodeApi::new();
    execute_prune_with_source(
        repo,
        stale_days,
        release_stale,
        reason,
        dry_run,
        format,
        &source,
        &api,
        &stderr_note,
    )
}

/// The injectable core of [`execute_prune`]: the issue-state source, the
/// OpenCode host, and the progress sink. The scan phase reads the host through
/// a per-invocation [`ScanHostCache`] so repeated directory questions across
/// repositories share their first answer; the removal recheck always calls
/// `api` directly. `progress` streams the concise notices (production passes
/// [`stderr_note`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_prune_with_source(
    repo: Option<&str>,
    stale_days: u32,
    release_stale: bool,
    reason: Option<String>,
    dry_run: bool,
    format: &str,
    source: &dyn IssueStateSource,
    api: &dyn crate::worktree::opencode::OpenCodeApi,
    progress: &dyn Fn(&str),
) -> i32 {
    // Wall duration of the command's own work; reported in text only, never as a
    // machine field.
    let started = Instant::now();
    let mut storage = match open_storage() {
        Ok(storage) => storage,
        Err(message) => return config_error(&message),
    };
    if let Err(message) = ensure_schema(&storage) {
        return config_error(&message);
    }
    let now = now_unix_secs();
    // One window feeds both the read-only lease scan and the transactional
    // recovery, so the two can never disagree about what "stale" means for
    // this invocation.
    let window = stale_window(now, stale_days);
    let runner = ProcessWorktreeRunner::new();
    // The scan reads through a per-invocation memo so repeated directory
    // questions across repositories reuse their first answer within this call;
    // every removal recheck below uses `api` directly.
    let scan_cache = ScanHostCache::new(api);

    // An applied recovery must be attributable, so a non-preview
    // `--release-stale` requires a non-empty reason. A preview records nothing
    // and may omit it.
    let requested_reason: Option<String> = reason
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    // Only a non-preview `--release-stale` applies the recovery. A preview
    // reports the same stale candidates and mutates nothing, so it never flips
    // a lease even when `--reason` was supplied.
    let apply_recovery = release_stale && !dry_run;
    if apply_recovery && requested_reason.is_none() {
        return structured_error(
            serde_json::json!({
                "kind": "argument",
                "operation": "worktree prune",
                "message": "worktree prune --release-stale requires a non-empty --reason",
            }),
            2,
        );
    }

    let scopes = match resolve_scopes(repo, &storage, &runner) {
        Ok(scopes) => scopes,
        Err(code) => return code,
    };

    // One global start notice, streamed before the first potentially slow scan
    // and never repeated per repository; the removal recheck adds at most one
    // truthful "verifying" notice per attempted deletion.
    progress(&format!(
        "scanning {} recorded {}",
        scopes.len(),
        if scopes.len() == 1 {
            "repository"
        } else {
            "repositories"
        }
    ));

    let mut outcomes: Vec<PruneRepoOutcome> = Vec::new();
    let mut totals = PruneTotals {
        repositories: 0,
        removed: 0,
        eligible: 0,
        absent: 0,
        kept_active: 0,
        kept_unknown: 0,
        kept_protected: 0,
        kept_issue: 0,
        failed: 0,
    };
    for scope in scopes {
        let (identity, path) = match scope {
            Scope::Skipped { identity, reason } => {
                outcomes.push(PruneRepoOutcome {
                    repo_identity: identity,
                    repo_path: None,
                    skipped_reason: Some(reason),
                    leases: None,
                    worktrees: None,
                });
                continue;
            }
            Scope::Resolved { identity, path } => (identity, path),
        };
        // The invocation-start snapshot: every row uncapped, and the set of
        // directories that hold an active lease right now. A same-call
        // recovery must not make any of those removable.
        let start_rows = match list_all_for_repo(&storage, &identity) {
            Ok(rows) => rows,
            Err(error) => return storage_error(&error),
        };
        let pinned_active = pinned_active_paths(&start_rows);
        let candidates = match stale_active_leases(&storage, &identity, window.before) {
            Ok(rows) => rows,
            Err(error) => return storage_error(&error),
        };
        let lease_summary = match requested_reason.as_deref().filter(|_| apply_recovery) {
            Some(reason) => match recover_stale_active_leases(
                &mut storage,
                &identity,
                window.before,
                reason,
                now,
            ) {
                Ok(flipped) => stale_recovery_summary(
                    &identity,
                    stale_days,
                    window.before,
                    now,
                    candidates,
                    &flipped,
                    reason,
                ),
                Err(error) => {
                    return structured_error(
                        serde_json::json!({
                            "kind": error.kind,
                            "operation": "worktree prune",
                            "message": error.message,
                        }),
                        1,
                    );
                }
            },
            None => stale_scan_summary(&identity, stale_days, window.before, now, candidates),
        };
        // Reuse the invocation-start row snapshot unless the applied recovery
        // actually flipped rows: only then can the lease table differ from
        // `start_rows`. The pinned-active set is always derived from the
        // pre-recovery snapshot, so a directory active at invocation start
        // stays protected either way.
        let refreshed_rows = if lease_summary.apply && lease_summary.released > 0 {
            match list_all_for_repo(&storage, &identity) {
                Ok(rows) => Some(rows),
                Err(error) => return storage_error(&error),
            }
        } else {
            None
        };
        let rows: &[LeaseRow] = refreshed_rows.as_deref().unwrap_or(&start_rows);
        // One verified move target per repository: the associated idle
        // sessions a removal returns are sent to this repository's main
        // checkout, and the target is resolved lazily so a pass that never
        // moves a session spawns no Git process.
        let worktree_summary = {
            let target = TargetResolver::new(&path, &identity);
            let scope = PruneScope {
                identity: &identity,
                repo_path: &path,
                rows,
                target: &target,
            };
            let mode = if dry_run {
                PruneMode::Report
            } else {
                PruneMode::Remove
            };
            prune_pass(
                &storage,
                &runner,
                &scan_cache,
                api,
                &scope,
                &pinned_active,
                source,
                stale_days,
                mode,
                progress,
            )
        };
        totals.repositories += 1;
        totals.removed += worktree_summary.removed;
        totals.eligible += worktree_summary.eligible;
        totals.absent += worktree_summary.absent;
        totals.kept_active += worktree_summary.kept_active;
        totals.kept_unknown += worktree_summary.kept_unknown;
        totals.kept_protected += worktree_summary.kept_protected;
        totals.kept_issue += worktree_summary.kept_issue;
        totals.failed += worktree_summary.failed;
        outcomes.push(PruneRepoOutcome {
            repo_identity: identity,
            repo_path: Some(path.display().to_string()),
            skipped_reason: None,
            leases: Some(lease_summary),
            worktrees: Some(worktree_summary),
        });
    }

    let aggregate = PruneAggregate {
        dry_run,
        remove: !dry_run,
        release_stale,
        stale_days,
        repos: outcomes,
        totals,
    };
    let failures = aggregate.totals.failed;
    let failure_code = i32::from(failures > 0);
    // The emitter follows the resolved output format. The scoped context
    // carries the global resolution and an explicit legacy override, so the
    // prune never decides its own format.
    match crate::output::context::effective_mode(format) {
        OutputMode::Json => {
            if print_json(&aggregate) != 0 {
                return 1;
            }
        }
        OutputMode::Text => {
            // The ticker is cleared before the result, so the live spinner
            // never overlaps the summary on an interactive terminal.
            crate::output::progress::clear();
            println!("{}", render_text(&aggregate, started.elapsed()));
        }
    }
    // A genuine removal failure keeps a non-zero exit in both output modes;
    // safety skips are not failures.
    failure_code
}

/// Resolve the repository scopes of one pass. An explicit `--repo` is a hard
/// argument/identity error when it cannot be used; in global mode every stored
/// identity is resolved through its main checkout and skipped with a reason
/// instead of guessed.
fn resolve_scopes(
    repo: Option<&str>,
    storage: &crate::infra::storage::Storage,
    runner: &ProcessWorktreeRunner,
) -> Result<Vec<Scope>, i32> {
    match repo {
        Some(raw) => {
            let path = PathBuf::from(raw);
            if !path.exists() {
                return Err(structured_error(
                    serde_json::json!({
                        "kind": "argument",
                        "message": format!("worktree prune --repo path does not exist: {}", path.display()),
                    }),
                    2,
                ));
            }
            match crate::worktree::repo_identity(runner, &path) {
                Ok(identity) => Ok(vec![Scope::Resolved { identity, path }]),
                Err(error) => Err(structured_error(
                    serde_json::json!({
                        "kind": error.kind,
                        "message": error.message,
                    }),
                    1,
                )),
            }
        }
        None => match repo_identities(storage) {
            Ok(identities) => Ok(identities
                .into_iter()
                .map(|identity| resolve_known_identity(runner, identity))
                .collect()),
            Err(error) => Err(storage_error(&error)),
        },
    }
}

fn resolve_known_identity(runner: &ProcessWorktreeRunner, identity: String) -> Scope {
    let Some(parent) = Path::new(&identity).parent().map(Path::to_path_buf) else {
        return Scope::Skipped {
            identity,
            reason: "repository identity has no checkout directory".to_owned(),
        };
    };
    if !parent.exists() {
        return Scope::Skipped {
            identity,
            reason: format!("checkout directory is missing: {}", parent.display()),
        };
    }
    match crate::worktree::repo_identity(runner, &parent) {
        Ok(resolved) if resolved == identity => Scope::Resolved {
            identity,
            path: parent,
        },
        Ok(resolved) => Scope::Skipped {
            identity,
            reason: format!(
                "checkout {} resolves to a different repository identity: {resolved}",
                parent.display()
            ),
        },
        Err(error) => Scope::Skipped {
            identity,
            reason: format!(
                "checkout {} cannot resolve its repository identity: {}",
                parent.display(),
                error.message
            ),
        },
    }
}

/// The text rendering of one prune pass: the curated report and the measured
/// elapsed time.
///
/// The report names what happened: the removals, the removals that failed,
/// the directories that were kept and why they were kept, the aggregate
/// counts, and the skipped repositories. The elapsed time is a measured
/// local fact and never a machine field.
pub(crate) fn render_text(aggregate: &PruneAggregate, elapsed: Duration) -> String {
    let context = crate::output::context::current_or_default();
    let value = serde_json::to_value(aggregate).unwrap_or(serde_json::Value::Null);
    let mut text = crate::output::human::render(
        crate::output::context::HumanView::WorktreePrune,
        &value,
        &context,
    );
    text.push_str(&format!("\nelapsed {:.3}s", elapsed.as_secs_f64().max(0.0)));
    text
}
