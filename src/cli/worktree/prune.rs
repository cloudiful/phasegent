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
//!   that mutates nothing and creates no lock file.
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
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::cli::{print_json, structured_error};
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
mod worktrees;

#[allow(unused_imports)]
pub(crate) use crate::cli::issue_closure::{
    ExplicitSelection, IssueStateSource, configured_source, configured_source_context,
    configured_source_prune, configured_source_scoped, configured_source_task_aware,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use worktrees::scan_worktree_candidates;
pub(crate) use worktrees::{
    DirectoryCandidate, PruneAction, PruneDisposition, PruneMode, PruneOutcome, PruneScope,
    PruneStage, PruneSummary, prune_pass, prune_pass_with_policy,
};

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
    execute_prune_with_source_verbose(repo, dry_run, format, verbose, &source, &api, &stderr_note)
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
    let started = Instant::now();
    let move_budget = MoveBudget::new();

    // An explicit `--repo` is validated as usable before the store is opened.
    let runner = ProcessWorktreeRunner::new();
    let explicit_scope = match repo {
        Some(raw) => match resolve_explicit_scope(raw, &runner) {
            Ok(scope) => Some(scope),
            Err(code) => return code,
        },
        None => None,
    };

    let storage = match open_pass_storage(dry_run) {
        Ok(Some(storage)) => storage,
        Ok(None) => {
            return emit_prune(&empty_aggregate(dry_run), format, verbose, started);
        }
        Err(message) => return config_error(&message),
    };

    let scopes = match explicit_scope {
        Some(scope) => vec![scope],
        None => match resolve_scopes(&storage, &runner) {
            Ok(scopes) => scopes,
            Err(code) => return code,
        },
    };

    let mode = if dry_run {
        PruneMode::Report
    } else {
        PruneMode::Remove
    };
    let tally = ProgressTally::new(scopes.len());
    progress(&tally.render(mode));

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
    for scope in scopes {
        let (identity, path) = match scope {
            Scope::Skipped { identity, reason } => {
                outcomes.push(PruneRepoOutcome {
                    repo_identity: identity,
                    repo_path: None,
                    skipped_reason: Some(reason),
                    worktrees: None,
                });
                tally.finish_repository(true);
                tally.set_stage(TallyStage::Complete);
                progress(&tally.render(mode));
                continue;
            }
            Scope::Resolved { identity, path } => (identity, path),
        };
        let records = match registry::load_records(&storage, Some(&identity)) {
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
            prune_pass_with_policy(
                &storage,
                &runner,
                api,
                api,
                &scope,
                source,
                mode,
                confirmation,
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
        totals.kept_protected += worktree_summary.kept_protected;
        totals.kept_issue += worktree_summary.kept_issue;
        totals.failed += worktree_summary.failed;
        outcomes.push(PruneRepoOutcome {
            repo_identity: identity,
            repo_path: Some(path.display().to_string()),
            skipped_reason: None,
            worktrees: Some(worktree_summary),
        });
    }

    emit_prune(
        &PruneAggregate {
            dry_run,
            remove: !dry_run,
            repos: outcomes,
            totals,
        },
        format,
        verbose,
        started,
    )
}

fn empty_aggregate(dry_run: bool) -> PruneAggregate {
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
        Ok(identity) => Ok(Scope::Resolved { identity, path }),
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
