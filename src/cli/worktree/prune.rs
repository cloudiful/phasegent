//! Executor and execution model for `worktree prune`.
//!
//! `worktree prune` is a global cleanup with a concise human-readable default
//! and an explicit machine-readable mode:
//!
//! * Without `--repo` the pass enumerates every repository identity the local
//!   lease store records (even outside Git) and resolves each through its main
//!   checkout, skipping the ones it cannot verify with a bounded reason instead
//!   of guessing. `--repo PATH` restricts the pass to one verified checkout.
//! * Removal is the default; `--dryrun`/`--dry-run` is the read-only preview
//!   that mutates nothing.
//! * The default stdout is concise English text; `--json` / `--format json`
//!   selects the aggregate JSON envelope.
//!
//! Since issue 916 a managed worktree is removed as soon as every issue it is
//! associated with is *confirmed closed* and no `active` lease references it —
//! there is no waiting period. Age never gates directory deletion;
//! `--stale-days` survives only for the explicit `--release-stale` recovery.
//! Closure is read from the configured provider through
//! [`issue_state`](self::issue_state) (the same current-configured-provider
//! model `issue sync` uses); a preview may read closure but never mutates.
//!
//! The pass never syncs a provider, never moves or releases a session, never
//! auto-releases an active lease, and never deletes a branch or lease row.
//! Directory removal reuses the shared candidate guards (see [`worktrees`]) so
//! a main checkout, an active peer's path, a dirty directory, or a
//! host-occupied directory is always kept.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::policy::Role;
use crate::worktree::leases::{
    ensure_schema, list_all_for_repo, recover_stale_active_leases, repo_identities,
    stale_active_leases, stale_window,
};
use crate::worktree::opencode::ProcessOpenCodeApi;
use crate::worktree::{ProcessWorktreeRunner, now_unix_secs};

use super::{config_error, open_storage, storage_error};

mod child_retirement;
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

use std::io::Write;

/// Write one concise, flushed `worktree prune:` progress notice to stderr, so
/// a caller watching the terminal sees work begin before a slow host or
/// issue-state check; stdout stays reserved for the final text or JSON
/// document. Tests inject their own `&dyn Fn(&str)` sink.
fn stderr_note(message: &str) {
    eprintln!("worktree prune: {message}");
    let _ = std::io::stderr().flush();
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
        let rows = match list_all_for_repo(&storage, &identity) {
            Ok(rows) => rows,
            Err(error) => return storage_error(&error),
        };
        let scope = PruneScope {
            identity: &identity,
            repo_path: &path,
            rows: &rows,
        };
        let mode = if dry_run {
            PruneMode::Report
        } else {
            PruneMode::Remove
        };
        progress(&format!(
            "repo {} ({})",
            identity,
            if dry_run { "preview" } else { "removal" }
        ));
        let worktree_summary = prune_pass(
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
        );
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
    if format == "json" {
        if print_json(&aggregate) != 0 {
            return 1;
        }
    } else {
        println!("{}", render_text(&aggregate));
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

/// Concise English default output: a short header, one line per repository
/// with directory-based counts, per-path detail only for removals, failures,
/// and protections, and a final aggregate summary.
pub(crate) fn render_text(aggregate: &PruneAggregate) -> String {
    let mode = if aggregate.dry_run {
        "dry-run preview"
    } else {
        "removal"
    };
    let mut lines = vec![format!(
        "worktree prune ({mode}; stale-days {} applies to --release-stale only)",
        aggregate.stale_days
    )];
    if aggregate.repos.is_empty() {
        lines.push("no repositories with recorded worktree leases".to_owned());
    }
    for repo in &aggregate.repos {
        if let Some(reason) = &repo.skipped_reason {
            lines.push(format!("skipped {}: {}", repo.repo_identity, reason));
            continue;
        }
        let path = repo.repo_path.as_deref().unwrap_or("?");
        lines.push(format!("repo {path} ({})", repo.repo_identity));
        if let Some(worktrees) = &repo.worktrees {
            if worktrees.scanned == 0 {
                lines.push("  no managed worktrees".to_owned());
            } else {
                lines.push(format!(
                    "  removed {}, eligible {}, absent {}, kept {}, removal failures {}",
                    worktrees.removed,
                    worktrees.eligible,
                    worktrees.absent,
                    worktrees.kept(),
                    worktrees.failed
                ));
                lines.push(format!(
                    "  kept by reason: active {}, protected {}, issue-state {}, unknown {}",
                    worktrees.kept_active,
                    worktrees.kept_protected,
                    worktrees.kept_issue,
                    worktrees.kept_unknown
                ));
                for action in &worktrees.actions {
                    if !matches!(
                        action.result.as_str(),
                        "pruned" | "prunable" | "failed" | "skipped_protected" | "skipped_issue"
                    ) {
                        continue;
                    }
                    let mut line = format!("  - {} {}", action.result, action.worktree_path);
                    if let Some(reason) = action.message.as_deref().filter(|text| !text.is_empty())
                    {
                        line.push_str(&format!(": {reason}"));
                    }
                    lines.push(line);
                    // Actual relocations are factual and print as `relocated`
                    // independently of the directory's final result; preview
                    // proposals print as `would relocate`. The two are never
                    // conflated, so a moved-then-kept directory still shows its
                    // confirmed moves.
                    if !action.relocated_children.is_empty() {
                        lines.push(format!(
                            "  -   relocated associated child session(s): {}",
                            action.relocated_children.join(", ")
                        ));
                    }
                    if !action.planned_children.is_empty() {
                        lines.push(format!(
                            "  -   would relocate associated child session(s): {}",
                            action.planned_children.join(", ")
                        ));
                    }
                }
            }
        }
        if let Some(leases) = &repo.leases
            && (leases.candidates > 0 || leases.released > 0)
        {
            lines.push(format!(
                "  leases: {} stale candidate(s), {} released",
                leases.candidates, leases.released
            ));
        }
    }
    let totals = &aggregate.totals;
    let kept = totals.kept_active + totals.kept_unknown + totals.kept_protected + totals.kept_issue;
    lines.push(format!(
        "summary: {} repositories, removed {}, eligible {}, absent {}, kept {}, removal failures {}",
        totals.repositories, totals.removed, totals.eligible, totals.absent, kept, totals.failed
    ));
    // Scope the failure count so it is not misread as a clean bill of health:
    // it counts only failed removal calls, and a kept/protected result (which
    // can be an unproven host answer) is not evidence that no host anomaly
    // occurred.
    lines.push(
        "removal failures count only failed `git worktree remove` calls; kept or protected \
         directories also cover unproven host answers and are not evidence of no anomaly"
            .to_owned(),
    );
    lines.join("\n")
}
