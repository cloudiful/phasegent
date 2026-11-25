//! Executor and execution model for `worktree prune`.
//!
//! `worktree prune` is a global cleanup whose output follows the global
//! output mode:
//!
//! * Without `--repo` the pass enumerates every repository identity the
//!   directory registry records and resolves each through its main checkout,
//!   skipping the ones it cannot verify with a bounded reason instead of
//!   guessing. `--repo PATH` restricts the pass to one verified checkout.
//! * Removal is the default; `--dryrun`/`--dry-run` is the read-only preview
//!   that moves and deletes nothing and creates no store, schema, operation
//!   lock, or registry row. The only write it performs is the bounded
//!   diagnostic log file, which is separate operational state.
//! * The output format follows the global `--output auto|text|json`
//!   selection.
//!
//! A registered directory is removed as soon as every associated task or
//! legacy issue is confirmed closed and the connected host reports no running
//! session in it. Age and registration alone never gate directory deletion.
//! The removal stage returns idle associated sessions to the verified main
//! checkout and re-reads the registry, guards, and closure evidence before the
//! no-force removal.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::infra::diag_log::{self, DiagLogger, EventKind, RunCounts};
use crate::infra::storage::Storage;
use crate::lifecycle::cleanup::TargetResolver;
use crate::lifecycle::cleanup::wait::{MoveBudget, MovePolicy};
use crate::output::context::OutputMode;
use crate::policy::Role;
use crate::worktree::opencode::ProcessOpenCodeApi;
use crate::worktree::registry;
use crate::worktree::{ProcessWorktreeRunner, WorktreeError};

use super::{config_error, open_storage, storage_error};

mod active_guard;
mod timing;
mod worktrees;

#[cfg(test)]
#[path = "prune/tests_logging.rs"]
mod tests_logging;

#[allow(unused_imports)]
pub(crate) use crate::cli::issue_closure::{
    ExplicitSelection, IssueStateSource, configured_source, configured_source_context,
    configured_source_prune, configured_source_scoped, configured_source_task_aware,
};
pub(crate) use worktrees::{
    DirectoryCandidate, PruneAction, PruneDisposition, PruneMode, PruneOutcome, PruneScope,
    PruneStage, PruneSummary, prune_pass, prune_pass_with_policy,
};

use self::timing::{TimedIssueStateSource, TimedOpenCodeApi};
pub(crate) use self::timing::{TimingReport, TimingStats};

fn stderr_note(message: &str) {
    crate::output::progress::stage(message);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TallyStage {
    Verifying,
    Deleting,
    Complete,
}

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

    fn record(&self, outcome: PruneOutcome) {
        self.directories_done.set(self.directories_done.get() + 1);
        match outcome {
            PruneOutcome::Pruned | PruneOutcome::Prunable => {
                self.removed.set(self.removed.get() + 1);
            }
            PruneOutcome::Absent => self.absent.set(self.absent.get() + 1),
            PruneOutcome::Failed => self.failed.set(self.failed.get() + 1),
            PruneOutcome::KeptProtected | PruneOutcome::KeptIssue => {
                self.kept.set(self.kept.get() + 1);
            }
            PruneOutcome::Noop => {}
        }
    }

    fn finish_repository(&self, skipped: bool) {
        self.repositories_done.set(self.repositories_done.get() + 1);
        if skipped {
            self.skipped.set(self.skipped.get() + 1);
        }
    }

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

fn progress_bar(done: usize, total: usize) -> String {
    const CELLS: usize = 10;
    let filled = done
        .saturating_mul(CELLS)
        .checked_div(total)
        .unwrap_or(0)
        .min(CELLS);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(CELLS - filled))
}

#[derive(Debug, Serialize)]
pub(crate) struct PruneTotals {
    pub repositories: usize,
    pub removed: usize,
    pub eligible: usize,
    pub absent: usize,
    pub kept_protected: usize,
    pub kept_issue: usize,
    pub failed: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct PruneRepoOutcome {
    pub repo_identity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<PruneSummary>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PruneAggregate {
    pub dry_run: bool,
    pub remove: bool,
    pub repos: Vec<PruneRepoOutcome>,
    pub totals: PruneTotals,
    /// The measured timings of this invocation. The stages overlap with the
    /// nested operations, so the durations are observations and never an
    /// additive total.
    pub timings: TimingReport,
}

enum Scope {
    Resolved { identity: String, path: PathBuf },
    Skipped { identity: String, reason: String },
}

pub(super) fn execute_prune(
    repo: Option<&str>,
    dry_run: bool,
    format: &str,
    verbose: bool,
    role: Role,
    context: &crate::cli::provider_context::ProviderContext,
) -> i32 {
    if let Err(error) = context.validate(role) {
        return crate::cli::provider_error(error);
    }
    let source = configured_source_prune(role, context.clone());
    let api = ProcessOpenCodeApi::new();
    // The diagnostic logger is opened before the scope is read, so a run that
    // fails early still leaves its boundary. It is best effort: a missing or
    // unwritable log directory costs the events, never the cleanup.
    let logger = diag_log::DiagLogger::open(diag_mode(dry_run), diag_warning).map(Rc::new);
    if let Some(logger) = &logger {
        logger.event(EventKind::RunStart).write();
    }
    execute_prune_pass(
        repo,
        dry_run,
        format,
        verbose,
        &source,
        &api,
        &stderr_note,
        logger,
    )
}

/// The diagnostic mode of one pass.
fn diag_mode(dry_run: bool) -> &'static str {
    if dry_run {
        diag_log::MODE_DRY_RUN
    } else {
        diag_log::MODE_REMOVE
    }
}

/// The one bounded warning a run reports when the log could not be written.
fn diag_warning(message: &str) {
    crate::cli::report_local_warnings("worktree prune", Some(message.to_owned()));
}

#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_prune_with_source(
    repo: Option<&str>,
    dry_run: bool,
    format: &str,
    source: &dyn IssueStateSource,
    api: &dyn crate::worktree::opencode::OpenCodeApi,
    progress: &dyn Fn(&str),
) -> i32 {
    execute_prune_with_source_verbose(repo, dry_run, format, false, source, api, progress)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_prune_with_source_verbose(
    repo: Option<&str>,
    dry_run: bool,
    format: &str,
    verbose: bool,
    source: &dyn IssueStateSource,
    api: &dyn crate::worktree::opencode::OpenCodeApi,
    progress: &dyn Fn(&str),
) -> i32 {
    execute_prune_pass(repo, dry_run, format, verbose, source, api, progress, None)
}

/// The pass itself. The optional logger belongs to the caller: the ordinary
/// command execution opts in, the unit tests and the other callers do not.
#[allow(clippy::too_many_arguments)]
fn execute_prune_pass(
    repo: Option<&str>,
    dry_run: bool,
    format: &str,
    verbose: bool,
    source: &dyn IssueStateSource,
    api: &dyn crate::worktree::opencode::OpenCodeApi,
    progress: &dyn Fn(&str),
    logger: Option<Rc<DiagLogger>>,
) -> i32 {
    let started = Instant::now();
    let move_budget = MoveBudget::new();
    // The timing sink belongs to this invocation alone: no global, no shared
    // state, and nothing that outlives the pass.
    let timing = match &logger {
        Some(logger) => TimingStats::with_logger(Rc::clone(logger)),
        None => TimingStats::new(),
    };

    // An explicit `--repo` is validated as usable before the store is opened.
    let runner = ProcessWorktreeRunner::new();
    let explicit_scope = match repo {
        Some(raw) => match resolve_explicit_scope(raw, &runner) {
            Ok(scope) => Some(scope),
            Err(code) => {
                finish_run(&logger, &timing, "error", None);
                return code;
            }
        },
        None => None,
    };

    let storage = match open_pass_storage(dry_run) {
        Ok(Some(storage)) => storage,
        Ok(None) => {
            let aggregate = empty_aggregate(dry_run, &timing);
            finish_run(
                &logger,
                &timing,
                "ok",
                Some(diag_counts(&aggregate.totals, 0)),
            );
            return emit_prune(&aggregate, format, verbose, started);
        }
        Err(message) => {
            finish_run(&logger, &timing, "error", None);
            return config_error(&message);
        }
    };

    let scopes = match explicit_scope {
        Some(scope) => vec![scope],
        None => match resolve_scopes(&storage, &runner) {
            Ok(scopes) => scopes,
            Err(code) => {
                finish_run(&logger, &timing, "error", None);
                return code;
            }
        },
    };

    let mode = if dry_run {
        PruneMode::Report
    } else {
        PruneMode::Remove
    };
    let tally = ProgressTally::new(scopes.len());
    progress(&tally.render(mode));

    // The boundary decorators forward every call unchanged and only measure
    // them; the pass below reads the decorated boundary exactly as it read
    // the plain one.
    let timed_source = TimedIssueStateSource::new(source, &timing);
    let timed_api = TimedOpenCodeApi::new(api, &timing);
    let measured_source: &dyn IssueStateSource = &timed_source;
    let measured_api: &dyn crate::worktree::opencode::OpenCodeApi = &timed_api;

    let mut outcomes: Vec<PruneRepoOutcome> = Vec::new();
    let mut totals = PruneTotals {
        repositories: 0,
        removed: 0,
        eligible: 0,
        absent: 0,
        kept_protected: 0,
        kept_issue: 0,
        failed: 0,
    };
    let scan_snapshot = std::cell::OnceCell::new();
    let mut scanned_total = 0usize;
    for (repo_index, scope) in scopes.into_iter().enumerate() {
        // The repository ordinal is a number, never an identity: the log
        // names which repository of the run it is looking at, and nothing
        // about where that repository lives.
        timing.set_repo(repo_index as u64 + 1);
        let (identity, path, skipped_reason) = match scope {
            Scope::Skipped { identity, reason } => {
                let path = Path::new(&identity)
                    .parent()
                    .unwrap_or(Path::new("/"))
                    .to_path_buf();
                (identity, path, Some(reason))
            }
            Scope::Resolved { identity, path } => (identity, path, None),
        };
        let records = match timing.measure_stage(timing::SCOPE_REGISTRY, || {
            registry::load_records(&storage, Some(&identity))
        }) {
            Ok(records) => records,
            Err(error) => {
                outcomes.push(PruneRepoOutcome {
                    repo_identity: identity,
                    repo_path: Some(path.display().to_string()),
                    skipped_reason: Some(format!(
                        "could not read the directory registry: {}",
                        error.message
                    )),
                    worktrees: None,
                });
                tally.finish_repository(true);
                tally.set_stage(TallyStage::Complete);
                progress(&tally.render(mode));
                continue;
            }
        };
        let target = TargetResolver::new(&path, &identity);
        let worktree_summary = {
            let scope = PruneScope {
                identity: &identity,
                repo_path: &path,
                records: &records,
                target: &target,
            };
            let confirmation = if dry_run {
                MovePolicy::NoWait
            } else {
                MovePolicy::Bounded(&move_budget)
            };
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
            worktrees::prune_pass_with_snapshot_timed(
                &storage,
                &runner,
                measured_api,
                measured_api,
                &scope,
                measured_source,
                mode,
                confirmation,
                &stages,
                &scan_snapshot,
                skipped_reason.is_some(),
                &timing,
            )
        };
        tally.finish_repository(skipped_reason.is_some());
        tally.set_stage(TallyStage::Complete);
        progress(&tally.render(mode));
        totals.repositories += 1;
        scanned_total += worktree_summary.scanned;
        totals.removed += worktree_summary.removed;
        totals.eligible += worktree_summary.eligible;
        totals.absent += worktree_summary.absent;
        totals.kept_protected += worktree_summary.kept_protected;
        totals.kept_issue += worktree_summary.kept_issue;
        totals.failed += worktree_summary.failed;
        outcomes.push(PruneRepoOutcome {
            repo_identity: identity,
            repo_path: Some(path.display().to_string()),
            skipped_reason,
            worktrees: Some(worktree_summary),
        });
    }

    let aggregate = PruneAggregate {
        dry_run,
        remove: !dry_run,
        repos: outcomes,
        totals,
        timings: timing.report(),
    };
    finish_run(
        &logger,
        &timing,
        "ok",
        Some(diag_counts(&aggregate.totals, scanned_total)),
    );
    emit_prune(&aggregate, format, verbose, started)
}

/// Write the end of one run. The event carries the safe aggregate counts and
/// the measured timings, never a path, an identity or a raw error.
fn finish_run(
    logger: &Option<Rc<DiagLogger>>,
    timing: &TimingStats,
    outcome: &'static str,
    counts: Option<RunCounts>,
) {
    let Some(logger) = logger else {
        return;
    };
    let mut event = logger.event(EventKind::RunEnd).outcome(outcome);
    event = event.timings(timing.diag_timings());
    if let Some(counts) = counts {
        event = event.counts(counts);
    }
    event.write();
}

/// The aggregate counts of one pass, in the diagnostic event shape.
fn diag_counts(totals: &PruneTotals, scanned: usize) -> RunCounts {
    RunCounts {
        scanned: scanned as u64,
        removed: totals.removed as u64,
        eligible: totals.eligible as u64,
        absent: totals.absent as u64,
        kept_protected: totals.kept_protected as u64,
        kept_issue: totals.kept_issue as u64,
        failed: totals.failed as u64,
    }
}

fn empty_aggregate(dry_run: bool, timing: &TimingStats) -> PruneAggregate {
    PruneAggregate {
        dry_run,
        remove: !dry_run,
        repos: Vec::new(),
        totals: PruneTotals {
            repositories: 0,
            removed: 0,
            eligible: 0,
            absent: 0,
            kept_protected: 0,
            kept_issue: 0,
            failed: 0,
        },
        timings: timing.report(),
    }
}

fn emit_prune(aggregate: &PruneAggregate, format: &str, verbose: bool, started: Instant) -> i32 {
    let failure_code = i32::from(aggregate.totals.failed > 0);
    match crate::output::context::effective_mode(format) {
        OutputMode::Json => {
            if print_json(aggregate) != 0 {
                return 1;
            }
        }
        OutputMode::Text => {
            crate::output::progress::clear();
            println!("{}", render_text(aggregate, started.elapsed(), verbose));
        }
    }
    failure_code
}

/// Open the store one pass needs. A preview opens only an existing file; a
/// removal opens write-capable storage and performs the one-time registry
/// upgrade. Nothing is created or migrated on a preview path.
fn open_pass_storage(dry_run: bool) -> Result<Option<Storage>, String> {
    if !dry_run {
        let storage = open_storage()?;
        registry::ensure_schema(&storage)?;
        return Ok(Some(storage));
    }
    Storage::open_existing_read_only()
}

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
        Ok(identity) => match resolve_known_identity(runner, identity) {
            resolved @ Scope::Resolved { .. } => Ok(resolved),
            Scope::Skipped { reason, .. } => Err(structured_error(
                serde_json::json!({ "kind": "repository", "message": reason }),
                1,
            )),
        },
        Err(error) => Err(structured_error(
            serde_json::json!({
                "kind": error.kind,
                "message": error.message,
            }),
            1,
        )),
    }
}

fn resolve_scopes(
    storage: &crate::infra::storage::Storage,
    runner: &ProcessWorktreeRunner,
) -> Result<Vec<Scope>, i32> {
    match registry::repo_identities(storage) {
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

pub(crate) fn render_text(aggregate: &PruneAggregate, elapsed: Duration, verbose: bool) -> String {
    let context = crate::output::context::current_or_default();
    let mut value = serde_json::to_value(aggregate).unwrap_or(serde_json::Value::Null);
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

const VERBOSE_MARKER: &str = "__verbose";

#[allow(dead_code)]
fn worktree_error(error: &WorktreeError) -> i32 {
    storage_error(error)
}
