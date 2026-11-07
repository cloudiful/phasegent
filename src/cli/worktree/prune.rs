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
//! [`crate::cli::issue_closure`] (the same current-configured-provider
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

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::infra::storage::Storage;
use crate::lifecycle::cleanup::TargetResolver;
use crate::lifecycle::cleanup::wait::{MoveBudget, MovePolicy};
use crate::output::context::OutputMode;
use crate::policy::Role;
use crate::worktree::leases::{
    StaleRecoveryScope, ensure_schema, list_all_for_repo, recover_stale_active_leases_for_scope,
    repo_identities, stale_active_leases, stale_window,
};
use crate::worktree::opencode::ProcessOpenCodeApi;
use crate::worktree::{LeaseRow, ProcessWorktreeRunner, now_unix_secs};

use super::{config_error, open_storage, storage_error};

mod active_guard;
mod host_cache;
mod leases;
mod worktrees;

#[allow(unused_imports)]
pub(crate) use crate::cli::issue_closure::{
    ExplicitSelection, IssueStateSource, configured_source, configured_source_context,
    configured_source_prune, configured_source_scoped, configured_source_task_aware,
};
use host_cache::ScanHostCache;
pub(crate) use leases::{ReleaseStaleAction, ReleaseStaleSummary};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use worktrees::scan_worktree_candidates;
pub(crate) use worktrees::{
    DirectoryCandidate, PruneAction, PruneDisposition, PruneMode, PruneOutcome, PruneScope,
    PruneStage, PruneSummary, pinned_active_paths, prune_pass, prune_pass_with_policy,
};

use leases::{stale_recovery_summary, stale_scan_summary};

/// Report one progress notice of the prune pass. The notice updates the
/// progress ticker of a human run; a machine run never sees a progress line.
fn stderr_note(message: &str) {
    crate::output::progress::stage(message);
}

/// The stage shown inside the current repository: verification of a
/// candidate, the actual deletion boundary, or the settled state after it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TallyStage {
    Verifying,
    Deleting,
    Complete,
}

/// The pass-wide progress tally. It is owned above the per-repository loop so
/// the repository bar and the directory counts are cumulative for the whole
/// invocation, never reset per repository. Only a completed action increments
/// the directory counts, and no candidate total is invented.
struct ProgressTally {
    repositories_total: Cell<usize>,
    repositories_done: Cell<usize>,
    skipped: Cell<usize>,
    directories_done: Cell<usize>,
    removed: Cell<usize>,
    absent: Cell<usize>,
    kept: Cell<usize>,
    failed: Cell<usize>,
    stage: Cell<TallyStage>,
}

impl ProgressTally {
    fn new(repositories_total: usize) -> Self {
        Self {
            repositories_total: Cell::new(repositories_total),
            repositories_done: Cell::new(0),
            skipped: Cell::new(0),
            directories_done: Cell::new(0),
            removed: Cell::new(0),
            absent: Cell::new(0),
            kept: Cell::new(0),
            failed: Cell::new(0),
            stage: Cell::new(TallyStage::Verifying),
        }
    }

    fn set_stage(&self, stage: TallyStage) {
        self.stage.set(stage);
    }

    /// Record one completed action. `Pruned` and `Prunable` are removal
    /// outcomes, everything a safety rule kept is a kept directory, and a
    /// failed action is its own bucket.
    fn record(&self, outcome: PruneOutcome) {
        self.directories_done.set(self.directories_done.get() + 1);
        match outcome {
            PruneOutcome::Pruned | PruneOutcome::Prunable => {
                self.removed.set(self.removed.get() + 1);
            }
            PruneOutcome::Absent => self.absent.set(self.absent.get() + 1),
            PruneOutcome::Failed => self.failed.set(self.failed.get() + 1),
            PruneOutcome::KeptActive
            | PruneOutcome::KeptUnknown
            | PruneOutcome::KeptProtected
            | PruneOutcome::KeptIssue => self.kept.set(self.kept.get() + 1),
            PruneOutcome::Noop => {}
        }
    }

    fn finish_repository(&self, skipped: bool) {
        self.repositories_done.set(self.repositories_done.get() + 1);
        if skipped {
            self.skipped.set(self.skipped.get() + 1);
        }
    }

    /// The one progress line. The bar tracks known repositories, while the
    /// directory counts stay cumulative and only move when an action returned.
    fn render(&self, mode: PruneMode) -> String {
        let (removed_label, kept_label) = if mode.is_report() {
            ("would remove", "would keep")
        } else {
            ("removed", "kept")
        };
        let done = self.repositories_done.get();
        let total = self.repositories_total.get();
        let stage = match self.stage.get() {
            TallyStage::Verifying => "verifying",
            TallyStage::Deleting => "deleting",
            TallyStage::Complete => "complete",
        };
        // The essential repository progress stays at the start so a truncated
        // narrow line still names where the pass is; the full outcome counts
        // remain in the final summary as before.
        let mut line = format!(
            "prune {} {done}/{total} repos {stage} {} dirs {removed_label} {}, \
             absent {}, {kept_label} {}, failed {}",
            progress_bar(done, total),
            self.directories_done.get(),
            self.removed.get(),
            self.absent.get(),
            self.kept.get(),
            self.failed.get(),
        );
        if self.skipped.get() > 0 {
            line.push_str(&format!(" skipped {}", self.skipped.get()));
        }
        line
    }
}

/// The bounded textual bar over known repositories. It only fills once the
/// counted repositories actually finished, so no line claims completion early.
fn progress_bar(done: usize, total: usize) -> String {
    const CELLS: usize = 10;
    let filled = done
        .saturating_mul(CELLS)
        .checked_div(total)
        .unwrap_or(0)
        .min(CELLS);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(CELLS - filled))
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
    Resolved {
        identity: String,
        path: PathBuf,
        /// The durable key of the physical repository, proven from its own
        /// checkout. `None` means it could not be proven, so a task-bearing
        /// lease row cannot be shown to belong to this repository.
        repo_key: Option<String>,
    },
    Skipped {
        identity: String,
        reason: String,
    },
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_prune(
    repo: Option<&str>,
    stale_days: u32,
    release_stale: bool,
    reason: Option<String>,
    dry_run: bool,
    format: &str,
    verbose: bool,
    role: Role,
    context: &crate::cli::provider_context::ProviderContext,
) -> i32 {
    // The complete context is validated before storage, recovery, mutation or
    // HTTP: incompatible Forgejo flags fail closed for any resolved selection,
    // while every provider resolves through the common chain.
    if let Err(error) = context.validate(role) {
        return crate::cli::provider_error(error);
    }
    // The complete raw invocation travels into the closure source, so an
    // omitted selection keeps endpoint overrides without being promoted to an
    // explicit provider and every explicit flag reaches validation.
    let source = configured_source_prune(role, context.clone());
    let legacy_numeric_leases = match context.capabilities(role) {
        Ok(capabilities) => capabilities.legacy_numeric_leases,
        Err(error) => return crate::cli::provider_error(error),
    };
    // Stale recovery is evidence-driven: a provider without legacy numeric
    // leases owns only its exact resolved scope, and a legacy-capable
    // provider keeps its unscoped numeric compatibility. A legacy-capable
    // selection never needs a scope, so it resolves no provider at all; a
    // preview resolves the exact scope read-only, before any store is opened.
    let explicit_selection = context.provider.is_some();
    let needs_exact_scope = explicit_selection && !legacy_numeric_leases;
    let resolved_scope = if needs_exact_scope {
        if dry_run {
            crate::cli::scope_guard::resolved_scope_read_only(role, context)
        } else {
            context
                .resolved_scope(role)
                .map(|scope| scope.map(|scope| (scope.provider.as_str(), scope.project)))
        }
        .ok()
        .flatten()
    } else {
        None
    };
    let recovery_scope = match (resolved_scope.as_ref(), legacy_numeric_leases) {
        (Some((provider, project)), false) => StaleRecoveryScope::Exact {
            provider,
            project: project.as_str(),
            task_id: None,
            instance: None,
        },
        (_, true) if explicit_selection => StaleRecoveryScope::LegacyOnly,
        _ => StaleRecoveryScope::Nothing,
    };
    let api = ProcessOpenCodeApi::new();
    execute_prune_with_source_verbose(
        repo,
        stale_days,
        release_stale,
        reason,
        dry_run,
        format,
        verbose,
        &source,
        &api,
        &stderr_note,
        recovery_scope,
        !legacy_numeric_leases,
    )
}

/// The injectable core of [`execute_prune`]: the issue-state source, the
/// OpenCode host, and the progress sink. The scan phase reads the host through
/// a per-invocation [`ScanHostCache`] so repeated directory questions across
/// repositories share their first answer; the removal recheck always calls
/// `api` directly. `progress` streams the concise notices (production passes
/// [`stderr_note`]).
///
/// This shape keeps the concise summary; [`execute_prune_with_source_verbose`]
/// is the same core with the verbose detail selection.
#[allow(dead_code)] // the test helpers call this shape
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
    recovery_scope: StaleRecoveryScope<'_>,
    scoped_only: bool,
) -> i32 {
    execute_prune_with_source_verbose(
        repo,
        stale_days,
        release_stale,
        reason,
        dry_run,
        format,
        false,
        source,
        api,
        progress,
        recovery_scope,
        scoped_only,
    )
}

/// The same injectable core with the verbose detail selection: the human text
/// lists every bounded action row instead of the fixed-size summary. The
/// machine document is the same either way.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_prune_with_source_verbose(
    repo: Option<&str>,
    stale_days: u32,
    release_stale: bool,
    reason: Option<String>,
    dry_run: bool,
    format: &str,
    verbose: bool,
    source: &dyn IssueStateSource,
    api: &dyn crate::worktree::opencode::OpenCodeApi,
    progress: &dyn Fn(&str),
    recovery_scope: StaleRecoveryScope<'_>,
    scoped_only: bool,
) -> i32 {
    // Wall duration of the command's own work; reported in text only, never as a
    // machine field.
    let started = Instant::now();
    // A provider without legacy numeric leases acts on exact scopes only.
    let legacy_numeric_leases = !scoped_only;
    // One scheduling budget for every pending move of a removal invocation;
    // the deadline begins at the first pending accepted move and never resets
    // per directory or session. The preview always keeps the no-wait policy.
    let move_budget = MoveBudget::new();
    // An applied recovery must be attributable, so a non-preview
    // `--release-stale` requires a non-empty reason. A preview records nothing
    // and may omit it. The refusal is decided before any store is opened, so a
    // refused invocation never creates or migrates one.
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

    // An explicit `--repo` is validated as usable before the store is opened:
    // a missing, non-repository, or mistyped existing directory is an argument
    // error, not a successful empty preview. The identity check is Git-only and
    // never opens or initializes the store.
    let runner = ProcessWorktreeRunner::new();
    let explicit_scope = match repo {
        Some(raw) => match resolve_explicit_scope(raw, &runner) {
            Ok(scope) => Some(scope),
            Err(code) => return code,
        },
        None => None,
    };

    // A preview opens only an existing store and inspects the schema it reads;
    // an absent store or an absent lease table is the normal empty preview.
    let mut storage = match open_pass_storage(dry_run) {
        Ok(PassStorage::Ready(storage)) => storage,
        Ok(PassStorage::Empty) => {
            let aggregate = PruneAggregate {
                dry_run,
                remove: !dry_run,
                release_stale,
                stale_days,
                repos: Vec::new(),
                totals: PruneTotals {
                    repositories: 0,
                    removed: 0,
                    eligible: 0,
                    absent: 0,
                    kept_active: 0,
                    kept_unknown: 0,
                    kept_protected: 0,
                    kept_issue: 0,
                    failed: 0,
                },
            };
            return emit_prune(&aggregate, format, verbose, started);
        }
        Err(message) => return config_error(&message),
    };
    let now = now_unix_secs();
    // One window feeds both the read-only lease scan and the transactional
    // recovery, so the two can never disagree about what "stale" means for
    // this invocation.
    let window = stale_window(now, stale_days);
    // The scan reads through a per-invocation memo so repeated directory
    // questions across repositories reuse their first answer within this call;
    // every removal recheck below uses `api` directly.
    let scan_cache = ScanHostCache::new(api);

    let scopes = match explicit_scope {
        Some(scope) => vec![scope],
        None => match resolve_scopes(&storage, &runner) {
            Ok(scopes) => scopes,
            Err(code) => return code,
        },
    };

    // The mode of this invocation is fixed before the loop so every stage
    // message uses the same labels and the pass-wide tally stays cumulative.
    let mode = if dry_run {
        PruneMode::Report
    } else {
        PruneMode::Remove
    };
    // One pass-wide tally above the per-repository loop: the repository bar is
    // the only known total, the directory counts are cumulative, and only a
    // returned action increments them.
    let tally = ProgressTally::new(scopes.len());
    progress(&tally.render(mode));

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
        let (identity, path, repo_key) = match scope {
            Scope::Skipped { identity, reason } => {
                outcomes.push(PruneRepoOutcome {
                    repo_identity: identity,
                    repo_path: None,
                    skipped_reason: Some(reason),
                    leases: None,
                    worktrees: None,
                });
                // A skipped repository still counts as processed, with an
                // explicit skipped count so the bar is not silently inflated.
                tally.finish_repository(true);
                tally.set_stage(TallyStage::Complete);
                progress(&tally.render(mode));
                continue;
            }
            Scope::Resolved {
                identity,
                path,
                repo_key,
            } => (identity, path, repo_key),
        };
        // Validate the complete sidecar scope map before any mutation,
        // including the `--release-stale` recovery below and every provider
        // read or removal in the scan. A corrupt or unreadable map skips this
        // repository with a bounded reason and leaves leases, heartbeats,
        // worktrees, provider calls, cache, and session moves untouched; the
        // preview follows the same validation. The map is checked against the
        // repository's own verified durable key, so a task lease that is not
        // a member of this physical repository is refused here.
        let scope_map = match crate::worktree::lease_scope::scope_map_for_repo_with_key(
            &storage,
            &identity,
            repo_key.as_deref(),
        ) {
            Ok(scopes) => scopes,
            Err(error) => {
                outcomes.push(PruneRepoOutcome {
                    repo_identity: identity,
                    repo_path: Some(path.display().to_string()),
                    skipped_reason: Some(format!(
                        "could not validate lease scopes: {}",
                        error.message
                    )),
                    leases: None,
                    worktrees: None,
                });
                // An unsafe or corrupt scope map is a skipped repository like
                // any other: finish it in the tally without reading or
                // mutating its scope, so processed/total and the skipped count
                // match the final aggregate.
                tally.finish_repository(true);
                tally.set_stage(TallyStage::Complete);
                progress(&tally.render(mode));
                continue;
            }
        };
        // The invocation-start snapshot: every row uncapped, and the set of
        // directories that hold an active lease right now. A same-call
        // recovery must not make any of those removable.
        let start_rows = match list_all_for_repo(&storage, &identity) {
            Ok(rows) => rows,
            Err(error) => return storage_error(&error),
        };
        let pinned_active = pinned_active_paths(&start_rows);
        let mut candidates = match stale_active_leases(&storage, &identity, window.before) {
            Ok(rows) => rows,
            Err(error) => return storage_error(&error),
        };
        // Only rows this invocation's provider selection may own are shown
        // or released: a legacy-capable selection never flips a scoped lease,
        // and a provider without legacy numeric leases flips only its exact
        // resolved scope.
        candidates.retain(|row| recovery_scope.allows(scope_map.get(&row.lease_id)));
        let lease_summary = match requested_reason.as_deref().filter(|_| apply_recovery) {
            Some(reason) => match recover_stale_active_leases_for_scope(
                &mut storage,
                &identity,
                repo_key.as_deref(),
                window.before,
                reason,
                now,
                recovery_scope,
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
            let confirmation = if dry_run {
                MovePolicy::NoWait
            } else {
                MovePolicy::Bounded(&move_budget)
            };
            let provider_scope = source.provider_scope();
            // The typed sink is the narrow stage boundary: the pruning pass
            // names verification, the actual deletion moment, and one
            // completed outcome, and this adapter owns the cumulative tally and
            // the rendered line.
            let stages = |stage: PruneStage| {
                match stage {
                    PruneStage::Verifying { .. } => {
                        tally.set_stage(TallyStage::Verifying);
                    }
                    PruneStage::Deleting { .. } => {
                        tally.set_stage(TallyStage::Deleting);
                    }
                    PruneStage::Applied { outcome } => {
                        tally.record(outcome);
                        tally.set_stage(TallyStage::Verifying);
                    }
                }
                progress(&tally.render(mode));
            };
            prune_pass_with_policy(
                &storage,
                &runner,
                &scan_cache,
                api,
                &scope,
                &pinned_active,
                source,
                stale_days,
                mode,
                confirmation,
                provider_scope
                    .as_ref()
                    .map(|(provider, project)| (provider.as_str(), project.as_str())),
                legacy_numeric_leases,
                &stages,
            )
        };
        tally.finish_repository(false);
        tally.set_stage(TallyStage::Complete);
        progress(&tally.render(mode));
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
    emit_prune(&aggregate, format, verbose, started)
}

/// Emit one finished pass: the machine document, or the ticker cleared before
/// the text result. A genuine removal failure keeps a non-zero exit in both
/// output modes; safety skips are not failures.
fn emit_prune(aggregate: &PruneAggregate, format: &str, verbose: bool, started: Instant) -> i32 {
    let failure_code = i32::from(aggregate.totals.failed > 0);
    // The emitter follows the resolved output format. The scoped context
    // carries the global resolution and an explicit legacy override, so the
    // prune never decides its own format.
    match crate::output::context::effective_mode(format) {
        OutputMode::Json => {
            if print_json(aggregate) != 0 {
                return 1;
            }
        }
        OutputMode::Text => {
            // The ticker is cleared before the result, so the live spinner
            // never overlaps the summary on an interactive terminal.
            crate::output::progress::clear();
            println!("{}", render_text(aggregate, started.elapsed(), verbose));
        }
    }
    failure_code
}

/// The store a pass may read.
enum PassStorage {
    /// No existing store, or no lease table in it: a preview reports the
    /// normal empty aggregate and touches nothing.
    Empty,
    Ready(Storage),
}

/// The lease columns the read-only preview queries. An older store is a
/// bounded error, never a migration on a preview path.
const PREVIEW_LEASE_COLUMNS: &[&str] = &[
    "lease_id",
    "repo_identity",
    "issue",
    "session",
    "checkout_path",
    "worktree_path",
    "branch",
    "status",
    "created_at",
    "heartbeat_at",
    "release_reason",
];

/// Open the store one pass needs.
///
/// A real removal keeps the write-capable open and schema initialisation. A
/// preview opens only an existing file, and then only inspects the schema it
/// reads: no directory, file, table, migration or permission change happens
/// on this path.
fn open_pass_storage(dry_run: bool) -> Result<PassStorage, String> {
    if !dry_run {
        let storage = open_storage()?;
        ensure_schema(&storage)?;
        return Ok(PassStorage::Ready(storage));
    }
    let Some(storage) = Storage::open_existing_read_only()? else {
        return Ok(PassStorage::Empty);
    };
    if !storage.table_exists("worktree_leases")? {
        return Ok(PassStorage::Empty);
    }
    for column in PREVIEW_LEASE_COLUMNS {
        if !storage.table_has_column("worktree_leases", column)? {
            return Err(format!(
                "worktree prune preview needs a non-preview worktree command to migrate the lease store (missing '{column}')"
            ));
        }
    }
    if !storage.table_exists("worktree_lease_scopes")? {
        return Err(
            "worktree prune preview needs a non-preview worktree command to migrate the lease store (missing scope table)"
                .to_owned(),
        );
    }
    Ok(PassStorage::Ready(storage))
}

/// Resolve one explicit `--repo` selection. A missing path is an argument
/// error; an existing path that is not a usable Git repository is an identity
/// error. Both are decided from the checkout alone, so a preview never opens or
/// initializes the lease store to validate the selection.
fn resolve_explicit_scope(raw: &str, runner: &ProcessWorktreeRunner) -> Result<Scope, i32> {
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
        Ok(identity) => Ok(Scope::Resolved {
            repo_key: crate::worktree::lease_scope::verified_repo_key_for_identity(&identity),
            identity,
            path,
        }),
        Err(error) => Err(structured_error(
            serde_json::json!({
                "kind": error.kind,
                "message": error.message,
            }),
            1,
        )),
    }
}

/// Resolve every stored repository identity in global mode through its main
/// checkout, skipping with a reason instead of guessing.
fn resolve_scopes(
    storage: &crate::infra::storage::Storage,
    runner: &ProcessWorktreeRunner,
) -> Result<Vec<Scope>, i32> {
    match repo_identities(storage) {
        Ok(identities) => Ok(identities
            .into_iter()
            .map(|identity| resolve_known_identity(runner, identity))
            .collect()),
        Err(error) => Err(storage_error(&error)),
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
        Ok(resolved) if resolved == identity => {
            let repo_key = crate::worktree::lease_scope::verified_repo_key_for_identity(&identity);
            Scope::Resolved {
                identity,
                path: parent,
                repo_key,
            }
        }
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

/// The text rendering of one prune pass: the fixed-size summary, the optional
/// verbose detail rows, and the measured elapsed time.
///
/// The default summary is independent of the number of directories: the
/// aggregate counts, the kept-reason breakdown, and at most a few failure
/// examples. The verbose rendering adds every bounded action row. The machine
/// document is unaffected by either selection.
pub(crate) fn render_text(aggregate: &PruneAggregate, elapsed: Duration, verbose: bool) -> String {
    let context = crate::output::context::current_or_default();
    let mut value = serde_json::to_value(aggregate).unwrap_or(serde_json::Value::Null);
    // The verbose marker lives on the rendered copy only, so the machine
    // document stays exactly the aggregate.
    if verbose && let Some(object) = value.as_object_mut() {
        object.insert(VERBOSE_MARKER.to_owned(), serde_json::Value::Bool(true));
    }
    let mut text = crate::output::human::render(
        crate::output::context::HumanView::WorktreePrune,
        &value,
        &context,
    );
    text.push_str(&format!("\nelapsed {:.3}s", elapsed.as_secs_f64().max(0.0)));
    text
}

/// The marker the human prune renderer reads to select the verbose detail
/// rows. It is set on the rendered copy only.
const VERBOSE_MARKER: &str = "__verbose";
