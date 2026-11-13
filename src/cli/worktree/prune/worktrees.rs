//! Directory half of `worktree prune`: the read-only candidate scan and the
//! removal apply.
//!
//! The unit of candidacy is a registered physical directory. A directory is
//! removed only when
//!
//! 1. the local Git guards clear it (never the main checkout, never a dirty or
//!    untracked directory, preserving the narrow verified root `.cache`
//!    exception);
//! 2. the connected shared OpenCode service reports no currently running
//!    session in it, and an unusable host answer keeps it;
//! 3. every associated task or legacy issue is confirmed closed by
//!    [`IssueStateSource`][crate::cli::issue_closure::IssueStateSource];
//! 4. the removal stage proves the directory is free of sessions it may not
//!    touch and returns the associated idle ones it may;
//! 5. `git worktree remove` (no `--force`) succeeds after a fresh re-read of
//!    the registry and guards.
//!
//! Registration alone never vetoes removal: a confirmed closed historical task
//! is removable. An associated still-open task and current host occupancy do
//! protect the directory. A proven absent directory's registration and owned
//! associations are reclaimed after a fresh check. Branches are never deleted.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::report_local_warnings;
use crate::infra::storage::Storage;
use crate::lifecycle::cleanup::CleanupMode;
use crate::lifecycle::cleanup::TargetResolver;
use crate::lifecycle::cleanup::sessions::{self, FamilyOutcome};
use crate::lifecycle::cleanup::wait::MovePolicy;
use crate::worktree::git::worktree_remove;
use crate::worktree::opencode::OpenCodeApi;
use crate::worktree::registry::{self, DirectoryRecord};
use crate::worktree::{WorktreeRunner, canonical_or_self};

use super::active_guard;
use super::timing::{self, TimingStats};
use crate::cli::issue_closure::{IssueClosure, IssueStateSource, TaskScopeRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PruneMode {
    Report,
    Remove,
}

impl PruneMode {
    pub(crate) fn is_report(self) -> bool {
        matches!(self, PruneMode::Report)
    }
}

pub(crate) struct PruneScope<'a> {
    pub identity: &'a str,
    pub repo_path: &'a Path,
    pub records: &'a [DirectoryRecord],
    pub target: &'a TargetResolver<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PruneDisposition {
    MissingDirectory,
    SkippedProtected(String),
    SkippedIssue(String),
    Removable,
}

/// A rejection that keeps a directory, together with the fixed reason code
/// the output carries. Every rejection path names one of these codes, so a
/// kept directory never falls back to an unnamed protected state.
pub(crate) struct Rejection {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl Rejection {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DirectoryCandidate {
    pub canonical_path: PathBuf,
    pub worktree_path: String,
    pub branch: String,
    pub directory_id: String,
    pub issues: Vec<u64>,
    pub sessions: Vec<String>,
    pub disposition: PruneDisposition,
    pub cache: Option<crate::worktree::cache::CacheInventory>,
    expected: DirectoryRecord,
    reason_code: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PruneAction {
    pub worktree_path: String,
    pub branch: String,
    pub directory_id: String,
    pub issues: Vec<u64>,
    pub sessions: Vec<String>,
    pub prunable: bool,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relocated_children: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PruneSummary {
    pub scanned: usize,
    pub removed: usize,
    pub eligible: usize,
    pub absent: usize,
    pub kept_protected: usize,
    pub kept_issue: usize,
    pub failed: usize,
    pub dry_run: bool,
    pub actions: Vec<PruneAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PruneStage {
    Verifying { candidate: usize },
    Deleting { candidate: usize },
    Applied { outcome: PruneOutcome },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PruneOutcome {
    Pruned,
    Prunable,
    Absent,
    KeptProtected,
    KeptIssue,
    Failed,
    Noop,
}

fn classify_outcome(action: &PruneAction) -> PruneOutcome {
    match action.result.as_str() {
        "pruned" => PruneOutcome::Pruned,
        "prunable" => PruneOutcome::Prunable,
        "absent" => PruneOutcome::Absent,
        "skipped_protected" => PruneOutcome::KeptProtected,
        "skipped_issue" => PruneOutcome::KeptIssue,
        "failed" => PruneOutcome::Failed,
        _ => PruneOutcome::Noop,
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn scan_worktree_candidates(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
) -> Vec<DirectoryCandidate> {
    scan_with_paths(
        runner,
        api,
        scope,
        source,
        &std::cell::OnceCell::new(),
        false,
    )
}

fn scan_with_paths(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    snapshot: &std::cell::OnceCell<Result<active_guard::RunningSnapshot, String>>,
    absent_only: bool,
) -> Vec<DirectoryCandidate> {
    let mut candidates: Vec<DirectoryCandidate> = scope
        .records
        .iter()
        .filter_map(|record| {
            let path = Path::new(&record.worktree_path);
            // The main checkout is excluded only after the absence probe: a
            // proven absent main registration is a stale row that may be
            // reclaimed, while a main checkout that exists — or whose state
            // cannot be proven — is never a candidate.
            match path_absent(path) {
                Ok(true) => Some(classify_absent_only(record)),
                Ok(false) => {
                    if same_path(path, scope.repo_path) {
                        None
                    } else if absent_only {
                        Some(unavailable_repository_candidate(record))
                    } else {
                        Some(classify(runner, api, scope, source, record, snapshot))
                    }
                }
                Err(reason) => {
                    if same_path(path, scope.repo_path) {
                        None
                    } else {
                        Some(probe_error_candidate(record, reason))
                    }
                }
            }
        })
        .collect();
    candidates.sort_by(|a, b| a.canonical_path.cmp(&b.canonical_path));
    candidates
}

/// A registration whose directory exists while the repository root is
/// unavailable: the directory stays, and the reason says why.
fn unavailable_repository_candidate(record: &DirectoryRecord) -> DirectoryCandidate {
    let mut candidate = classify_absent_only(record);
    candidate.reason_code = Some("repository_unavailable");
    candidate.disposition = PruneDisposition::SkippedProtected(
        "repository checkout is unavailable; existing directory retained".to_owned(),
    );
    candidate
}

/// A registration whose path could not be inspected. It is never absent and
/// never removed.
fn probe_error_candidate(record: &DirectoryRecord, reason: String) -> DirectoryCandidate {
    let mut candidate = classify_absent_only(record);
    candidate.reason_code = Some("path_probe_error");
    candidate.disposition = PruneDisposition::SkippedProtected(reason);
    candidate
}

fn classify_absent_only(record: &DirectoryRecord) -> DirectoryCandidate {
    DirectoryCandidate {
        canonical_path: PathBuf::from(&record.worktree_path),
        worktree_path: record.worktree_path.clone(),
        branch: record.branch.clone(),
        directory_id: record.directory_id.clone(),
        issues: record.issue_numbers(),
        sessions: record.sessions.clone(),
        disposition: PruneDisposition::MissingDirectory,
        cache: None,
        expected: record.clone(),
        reason_code: None,
    }
}

fn classify(
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    record: &DirectoryRecord,
    snapshot: &std::cell::OnceCell<Result<active_guard::RunningSnapshot, String>>,
) -> DirectoryCandidate {
    let canonical = canonical_or_self(Path::new(&record.worktree_path));
    let mut candidate = DirectoryCandidate {
        canonical_path: canonical.clone(),
        worktree_path: record.worktree_path.clone(),
        branch: record.branch.clone(),
        directory_id: record.directory_id.clone(),
        issues: record.issue_numbers(),
        sessions: record.sessions.clone(),
        disposition: PruneDisposition::SkippedProtected(String::new()),
        cache: None,
        expected: record.clone(),
        reason_code: None,
    };
    match path_absent(Path::new(&record.worktree_path)) {
        Ok(true) => {
            candidate.disposition = PruneDisposition::MissingDirectory;
            return candidate;
        }
        Ok(false) => {}
        Err(reason) => {
            candidate.reason_code = Some("path_probe_error");
            candidate.disposition = PruneDisposition::SkippedProtected(reason);
            return candidate;
        }
    }
    if same_path(&canonical, scope.repo_path) {
        candidate.reason_code = Some("main_checkout");
        candidate.disposition =
            PruneDisposition::SkippedProtected("the main checkout is never removed".to_owned());
        return candidate;
    }
    match local_guard(runner, &canonical) {
        Err((code, reason)) => {
            candidate.reason_code = Some(code);
            candidate.disposition = PruneDisposition::SkippedProtected(reason);
            return candidate;
        }
        Ok(cache) => candidate.cache = cache,
    }
    match snapshot.get_or_init(|| active_guard::RunningSnapshot::capture(api)) {
        Err(_) => {
            candidate.reason_code = Some("host_lookup_error");
            candidate.disposition =
                PruneDisposition::SkippedProtected("running-session lookup failed".to_owned());
            return candidate;
        }
        Ok(snapshot) => {
            if let Some(reason) = snapshot.keep_reason(&canonical) {
                candidate.reason_code = Some("running_session");
                candidate.disposition = PruneDisposition::SkippedProtected(reason);
                return candidate;
            }
        }
    }
    match closure_of(source, scope, record) {
        IssueClosure::Closed => candidate.disposition = PruneDisposition::Removable,
        IssueClosure::Keep(_) => unreachable!("closure lookup failures are normalized"),
        IssueClosure::LookupFailure(category) => {
            candidate.reason_code = Some(category.code());
            candidate.disposition = PruneDisposition::SkippedIssue(category.hint().to_owned());
        }
        IssueClosure::Open(reason) => {
            candidate.reason_code = Some("issue_open");
            candidate.disposition = PruneDisposition::SkippedIssue(reason);
        }
        IssueClosure::Unattributed(reason) => {
            candidate.reason_code = Some("unattributed_legacy");
            candidate.disposition = PruneDisposition::SkippedIssue(reason);
        }
    }
    candidate
}

pub(super) fn path_absent(path: &Path) -> Result<bool, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("directory path is not an absolute normalized path".to_owned());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("directory path contains a symbolic link".to_owned());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(_) => return Err("directory path could not be inspected".to_owned()),
        }
    }
    Ok(false)
}

fn same_path(left: &Path, right: &Path) -> bool {
    canonical_or_self(left) == canonical_or_self(right)
}

/// The local Git guards: existing, not the main checkout, and clean. The
/// narrow root `.cache` exception is carried forward as the inventory the
/// removal stage must still match.
fn local_guard(
    runner: &dyn WorktreeRunner,
    directory: &Path,
) -> Result<Option<crate::worktree::cache::CacheInventory>, (&'static str, String)> {
    match crate::worktree::is_clean(runner, directory) {
        Ok(true) => Ok(None),
        Ok(false) => match crate::worktree::cache::inspect(runner, directory) {
            Ok(crate::worktree::cache::CacheInspection::CacheOnly(inventory)) => {
                Ok(Some(inventory))
            }
            Ok(crate::worktree::cache::CacheInspection::NotCacheOnly) => Err((
                "dirty",
                "the worktree is dirty or has untracked files".to_owned(),
            )),
            Err(reason) => Err(("probe_error", reason)),
        },
        Err(error) => Err((
            "probe_error",
            format!(
                "the cleanliness probe failed: {}",
                crate::worktree::bounded(&error.message)
            ),
        )),
    }
}

fn closure_of(
    source: &dyn IssueStateSource,
    scope: &PruneScope<'_>,
    record: &DirectoryRecord,
) -> IssueClosure {
    if record.issue_numbers().is_empty() {
        return IssueClosure::Unattributed(
            "the directory has no associated issue to prove closed".to_owned(),
        );
    }
    let mut keep: Vec<IssueClosure> = Vec::new();
    for task in &record.tasks {
        let instance = task.instance.as_deref().unwrap_or_default();
        if instance.is_empty() || task.provider.is_none() || task.project.is_none() {
            keep.push(IssueClosure::Unattributed(format!(
                "issue {} durable task identity is incomplete",
                task.issue
            )));
            continue;
        }
        let verdict = source.own_task_closure(
            scope.repo_path,
            TaskScopeRef {
                task_id: task.task_id,
                provider: task.provider.as_deref().unwrap_or_default(),
                project: task.project.as_deref().unwrap_or_default(),
                instance,
            },
            task.issue,
        );
        if !verdict.is_closed() {
            keep.push(verdict.safe_lookup_failure());
        }
    }
    for issue in &record.legacy_issues {
        let verdict =
            if issue.provider.is_empty() || issue.project.is_empty() || issue.external_id == 0 {
                // No recorded provenance: the invocation's default or explicit
                // provider is not historical ownership, so keep before any read.
                IssueClosure::Unattributed(format!(
                    "issue {} has no corroborated provider/project/instance provenance",
                    issue.issue
                ))
            } else {
                match crate::worktree::WorktreeScope::new(
                    &issue.provider,
                    &issue.project,
                    issue.external_id,
                ) {
                    Ok(worktree_scope) => {
                        source.scoped_closure(scope.repo_path, issue.issue, Some(&worktree_scope))
                    }
                    Err(_) => IssueClosure::Unattributed(format!(
                        "issue {} legacy scope evidence is not canonical",
                        issue.issue
                    )),
                }
            };
        if !verdict.is_closed() {
            keep.push(verdict.safe_lookup_failure());
        }
    }
    keep.into_iter().next().unwrap_or(IssueClosure::Closed)
}

#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn prune_pass(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    scan_api: &dyn OpenCodeApi,
    fresh_api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    mode: PruneMode,
    stages: &dyn Fn(PruneStage),
) -> PruneSummary {
    prune_pass_with_policy(
        storage,
        runner,
        scan_api,
        fresh_api,
        scope,
        source,
        mode,
        MovePolicy::NoWait,
        stages,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prune_pass_with_policy(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    scan_api: &dyn OpenCodeApi,
    fresh_api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    mode: PruneMode,
    confirmation: MovePolicy<'_>,
    stages: &dyn Fn(PruneStage),
) -> PruneSummary {
    prune_pass_with_snapshot(
        storage,
        runner,
        scan_api,
        fresh_api,
        scope,
        source,
        mode,
        confirmation,
        stages,
        &std::cell::OnceCell::new(),
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prune_pass_with_snapshot(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    scan_api: &dyn OpenCodeApi,
    fresh_api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    mode: PruneMode,
    confirmation: MovePolicy<'_>,
    stages: &dyn Fn(PruneStage),
    snapshot: &std::cell::OnceCell<Result<active_guard::RunningSnapshot, String>>,
    absent_only: bool,
) -> PruneSummary {
    prune_pass_with_snapshot_timed(
        storage,
        runner,
        scan_api,
        fresh_api,
        scope,
        source,
        mode,
        confirmation,
        stages,
        snapshot,
        absent_only,
        &TimingStats::new(),
    )
}

/// The prune pass with the invocation's own timing sink.
#[allow(clippy::too_many_arguments)]
pub(super) fn prune_pass_with_snapshot_timed(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    scan_api: &dyn OpenCodeApi,
    fresh_api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    mode: PruneMode,
    confirmation: MovePolicy<'_>,
    stages: &dyn Fn(PruneStage),
    snapshot: &std::cell::OnceCell<Result<active_guard::RunningSnapshot, String>>,
    absent_only: bool,
    timing: &TimingStats,
) -> PruneSummary {
    // One per-repository operation lock covers the whole removal pass: fresh
    // checks, Git mutation, and the short registry transactions share it.
    // A preview and every other read path create no lock file.
    let _lock = if mode.is_report() {
        None
    } else {
        match crate::worktree::operation_lock::acquire(scope.identity) {
            Ok(lock) => Some(lock),
            Err(error) => {
                // The lock refused every reclamation in this pass, so an
                // absent main registration is reported as kept with the lock
                // reason instead of as a reclaimed absence.
                let actions = scope
                    .records
                    .iter()
                    .filter(|record| {
                        !same_path(Path::new(&record.worktree_path), scope.repo_path)
                            || path_absent(Path::new(&record.worktree_path)).unwrap_or(false)
                    })
                    .map(|record| PruneAction {
                        worktree_path: record.worktree_path.clone(),
                        branch: record.branch.clone(),
                        directory_id: record.directory_id.clone(),
                        issues: record.issue_numbers(),
                        sessions: record.sessions.clone(),
                        prunable: false,
                        result: "skipped_protected".to_owned(),
                        message: Some(error.message.clone()),
                        reason_code: Some("operation_lock"),
                        relocated_children: Vec::new(),
                    })
                    .collect::<Vec<_>>();
                let total = actions.len();
                return summarize(actions, total, mode);
            }
        }
    };
    let candidates = timing.measure_stage(timing::SCAN, || {
        scan_with_paths(runner, scan_api, scope, source, snapshot, absent_only)
    });
    let total = candidates.len();
    let mut actions: Vec<PruneAction> = Vec::with_capacity(total);
    for (index, candidate) in candidates.iter().enumerate() {
        stages(PruneStage::Verifying {
            candidate: index + 1,
        });
        let action = apply_candidate(
            storage,
            runner,
            fresh_api,
            scope,
            source,
            candidate,
            mode,
            confirmation,
            stages,
            index + 1,
            timing,
        );
        stages(PruneStage::Applied {
            outcome: classify_outcome(&action),
        });
        actions.push(action);
    }
    summarize(actions, total, mode)
}

#[allow(clippy::too_many_arguments)]
fn apply_candidate(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    candidate: &DirectoryCandidate,
    mode: PruneMode,
    confirmation: MovePolicy<'_>,
    stages: &dyn Fn(PruneStage),
    candidate_index: usize,
    timing: &TimingStats,
) -> PruneAction {
    let mut action = PruneAction {
        worktree_path: candidate.worktree_path.clone(),
        branch: candidate.branch.clone(),
        directory_id: candidate.directory_id.clone(),
        issues: candidate.issues.clone(),
        sessions: candidate.sessions.clone(),
        prunable: false,
        result: "noop".to_owned(),
        message: None,
        reason_code: candidate.reason_code,
        relocated_children: Vec::new(),
    };
    match &candidate.disposition {
        PruneDisposition::SkippedProtected(reason) => {
            action.result = "skipped_protected".to_owned();
            action.message = Some(reason.clone());
        }
        PruneDisposition::SkippedIssue(reason) => {
            action.result = "skipped_issue".to_owned();
            action.message = Some(reason.clone());
        }
        PruneDisposition::MissingDirectory => {
            action.result = "absent".to_owned();
            if mode.is_report() {
                action.message =
                    Some("dry-run: would reclaim absent directory registration".to_owned());
            } else {
                let check = timing.measure_stage(timing::CACHE_DELETE_RECLAIM, || {
                    registry::load_records(storage, Some(scope.identity))
                        .map_err(|_| {
                            Rejection::new(
                                "registry_read_failed",
                                "could not re-read the directory registry",
                            )
                        })
                        .and_then(|records| {
                            if !records.iter().any(|record| record == &candidate.expected) {
                                return Err(Rejection::new(
                                    "registration_changed",
                                    "directory registration changed after the scan",
                                ));
                            }
                            if !path_absent(Path::new(&candidate.worktree_path))
                                .map_err(|reason| Rejection::new("path_probe_error", reason))?
                            {
                                return Err(Rejection::new(
                                    "path_reappeared",
                                    "directory path reappeared after the scan",
                                ));
                            }
                            registry::remove_directory(
                                storage,
                                scope.identity,
                                &candidate.directory_id,
                            )
                            .map_err(|_| {
                                Rejection::new(
                                    "registry_cleanup_failed",
                                    "directory registration cleanup failed",
                                )
                            })
                            .and_then(|removed| {
                                if removed == 1 {
                                    Ok(())
                                } else {
                                    Err(Rejection::new(
                                        "registry_record_missing",
                                        "directory registration disappeared",
                                    ))
                                }
                            })
                        })
                });
                match check {
                    Ok(()) => {
                        action.message = Some("absent directory registration reclaimed".to_owned())
                    }
                    Err(rejection) => {
                        action.result = "skipped_protected".to_owned();
                        action.reason_code = Some(rejection.code);
                        action.message = Some(rejection.message);
                    }
                }
            }
        }
        PruneDisposition::Removable => {
            if mode.is_report() {
                match preview_family(api, scope, candidate) {
                    FamilyOutcome::Keep(reason) => {
                        action.result = "skipped_protected".to_owned();
                        action.reason_code = Some("session_family");
                        action.message = Some(reason);
                    }
                    FamilyOutcome::Releasable { sessions } => {
                        action.prunable = true;
                        action.result = "prunable".to_owned();
                        action.message = Some(dry_run_message(sessions.len()));
                    }
                    _ => {
                        action.prunable = true;
                        action.result = "prunable".to_owned();
                        action.message = Some("dry-run: would call git worktree remove".to_owned());
                    }
                }
            } else {
                match timing.measure_stage(timing::VERIFICATION, || {
                    recheck(storage, runner, api, scope, source, candidate)
                }) {
                    Err(rejection) => {
                        action.result = "skipped_protected".to_owned();
                        action.reason_code = Some(rejection.code);
                        action.message = Some(format!("kept on re-check: {}", rejection.message));
                    }
                    Ok((associated, fresh_cache)) => {
                        if fresh_cache != candidate.cache {
                            action.result = "skipped_protected".to_owned();
                            action.reason_code = Some("cache_changed");
                            action.message = Some(
                                "kept on re-check: the root .cache inventory changed before the moves"
                                    .to_owned(),
                            );
                        } else {
                            let family = timing.measure_stage(timing::FAMILY_MOVE, || {
                                sessions::finalize_family_with(
                                    api,
                                    CleanupMode::Prune,
                                    scope.target,
                                    &candidate.canonical_path,
                                    &associated,
                                    None,
                                    confirmation,
                                )
                            });
                            match family {
                                FamilyOutcome::Keep(reason) => {
                                    action.result = "skipped_protected".to_owned();
                                    action.reason_code = Some("session_family");
                                    action.message = Some(format!("kept on re-check: {reason}"));
                                }
                                _ => {
                                    if candidate.cache.is_some() {
                                        // A cache-bearing candidate keeps two
                                        // fresh mutation boundaries. The first
                                        // one runs after the family move and
                                        // before the cache is deleted: the
                                        // deletion is destructive and cannot be
                                        // undone, so the evidence must be fresh
                                        // before it. The second one runs after
                                        // the actual clearance and before the
                                        // removal.
                                        let pre_clearance =
                                            timing.measure_stage(timing::VERIFICATION, || {
                                                post_move_recheck(
                                                    storage, runner, api, scope, source, candidate,
                                                )
                                            });
                                        match pre_clearance {
                                            Err(rejection) => {
                                                action.result = "skipped_protected".to_owned();
                                                action.reason_code = Some(rejection.code);
                                                action.message = Some(format!(
                                                    "kept on re-check: {}",
                                                    rejection.message
                                                ));
                                            }
                                            Ok(fresh) if fresh != candidate.cache => {
                                                action.result = "skipped_protected".to_owned();
                                                action.reason_code = Some("cache_changed");
                                                action.message = Some(
                                                    "kept on re-check: the root .cache inventory                                                      changed before the cleanup"
                                                        .to_owned(),
                                                );
                                            }
                                            Ok(_) => {
                                                let cleared = timing.measure_stage(
                                                    timing::CACHE_DELETE_RECLAIM,
                                                    || {
                                                        crate::worktree::cache::clear_cache_only(
                                                            runner,
                                                            Path::new(&candidate.worktree_path),
                                                            candidate.cache.as_ref(),
                                                        )
                                                    },
                                                );
                                                match cleared {
                                                    Err(reason) => {
                                                        action.result =
                                                            "skipped_protected".to_owned();
                                                        action.reason_code =
                                                            Some("cache_cleanup_failed");
                                                        action.message = Some(format!(
                                                            "kept on re-check: {reason}"
                                                        ));
                                                    }
                                                    Ok(removed_cache) => {
                                                        let post_clearance = timing.measure_stage(
                                                            timing::VERIFICATION,
                                                            || {
                                                                post_move_recheck(
                                                                    storage, runner, api, scope,
                                                                    source, candidate,
                                                                )
                                                            },
                                                        );
                                                        match post_clearance {
                                                            Err(rejection) => {
                                                                action.result =
                                                                    "skipped_protected".to_owned();
                                                                action.reason_code =
                                                                    Some(rejection.code);
                                                                action.message = Some(format!(
                                                                    "kept after cache cleanup: {}",
                                                                    rejection.message
                                                                ));
                                                            }
                                                            Ok(Some(_)) => {
                                                                action.result =
                                                                    "skipped_protected".to_owned();
                                                                action.reason_code =
                                                                    Some("cache_reappeared");
                                                                action.message = Some(
                                                                    "kept after cache cleanup: a root                                                                      .cache reappeared before the                                                                      worktree removal"
                                                                        .to_owned(),
                                                                );
                                                            }
                                                            Ok(None) => {
                                                                stages(PruneStage::Deleting {
                                                                    candidate: candidate_index,
                                                                });
                                                                apply_removal(
                                                                    storage,
                                                                    runner,
                                                                    scope,
                                                                    candidate,
                                                                    &mut action,
                                                                    timing,
                                                                    removed_cache,
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        // A candidate without a root .cache has
                                        // nothing destructive to authorize: the
                                        // cleanliness probe is not a mutation,
                                        // so it runs before the one final fresh
                                        // check, and no external Git process
                                        // sits between that check and the
                                        // removal.
                                        let probe = timing.measure_stage(
                                            timing::CACHE_DELETE_RECLAIM,
                                            || {
                                                crate::worktree::cache::clear_cache_only(
                                                    runner,
                                                    Path::new(&candidate.worktree_path),
                                                    None,
                                                )
                                            },
                                        );
                                        match probe {
                                            Err(reason) => {
                                                action.result = "skipped_protected".to_owned();
                                                action.reason_code =
                                                    Some("cleanliness_probe_failed");
                                                action.message =
                                                    Some(format!("kept on re-check: {reason}"));
                                            }
                                            Ok(_) => {
                                                let final_check = timing.measure_stage(
                                                    timing::VERIFICATION,
                                                    || {
                                                        post_move_recheck(
                                                            storage, runner, api, scope, source,
                                                            candidate,
                                                        )
                                                    },
                                                );
                                                match final_check {
                                                    Err(rejection) => {
                                                        action.result =
                                                            "skipped_protected".to_owned();
                                                        action.reason_code = Some(rejection.code);
                                                        action.message = Some(format!(
                                                            "kept on re-check: {}",
                                                            rejection.message
                                                        ));
                                                    }
                                                    Ok(Some(_)) => {
                                                        action.result =
                                                            "skipped_protected".to_owned();
                                                        action.reason_code =
                                                            Some("cache_reappeared");
                                                        action.message = Some(
                                                            "kept on re-check: a root .cache                                                              appeared before the worktree removal"
                                                                .to_owned(),
                                                        );
                                                    }
                                                    Ok(None) => {
                                                        stages(PruneStage::Deleting {
                                                            candidate: candidate_index,
                                                        });
                                                        apply_removal(
                                                            storage,
                                                            runner,
                                                            scope,
                                                            candidate,
                                                            &mut action,
                                                            timing,
                                                            None,
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    action
}

fn associated_sessions(candidate: &DirectoryCandidate) -> Vec<String> {
    let mut ids: Vec<String> = candidate
        .sessions
        .iter()
        .map(|session| session.trim().to_owned())
        .filter(|session| crate::worktree::opencode::is_opencode_session(session))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn preview_family(
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    candidate: &DirectoryCandidate,
) -> FamilyOutcome {
    sessions::finalize_family(
        api,
        CleanupMode::Preview,
        scope.target,
        &candidate.canonical_path,
        &associated_sessions(candidate),
        None,
    )
}

fn dry_run_message(sessions: usize) -> String {
    match sessions {
        0 => "dry-run: would call git worktree remove".to_owned(),
        1 => "dry-run: would return 1 idle OpenCode session to the main checkout, then call git worktree remove".to_owned(),
        many => format!(
            "dry-run: would return {many} idle OpenCode sessions to the main checkout, then call git worktree remove"
        ),
    }
}

fn recheck(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    candidate: &DirectoryCandidate,
) -> Result<(Vec<String>, Option<crate::worktree::cache::CacheInventory>), Rejection> {
    let fresh = registry::load_records(storage, Some(scope.identity)).map_err(|error| {
        Rejection::new(
            "registry_read_failed",
            format!(
                "could not re-read the directory registry: {}",
                error.message
            ),
        )
    })?;
    if canonical_or_self(Path::new(&candidate.worktree_path)) != candidate.canonical_path {
        return Err(Rejection::new(
            "path_changed",
            "the directory path changed its target",
        ));
    }
    let fresh_scope = PruneScope {
        identity: scope.identity,
        repo_path: scope.repo_path,
        records: &fresh,
        target: scope.target,
    };
    let fresh_record = fresh
        .iter()
        .find(|record| record.directory_id == candidate.directory_id)
        .ok_or_else(|| {
            Rejection::new(
                "registry_record_missing",
                "the candidate's registry record is gone",
            )
        })?;
    source.invalidate(scope.repo_path, &fresh_record.issue_numbers());
    let classified = classify(
        runner,
        api,
        &fresh_scope,
        source,
        fresh_record,
        &std::cell::OnceCell::new(),
    );
    if let PruneDisposition::Removable = classified.disposition {
        Ok((associated_sessions(&classified), classified.cache))
    } else {
        Err(classification_rejection(&classified))
    }
}

/// The rejection a classification carries: its own reason code, or the
/// unnamed protected state when it has none.
fn classification_rejection(classified: &DirectoryCandidate) -> Rejection {
    let code = match classified.reason_code {
        Some(code) => code,
        None => match classified.disposition {
            PruneDisposition::MissingDirectory => "directory_vanished",
            _ => "other_protected",
        },
    };
    Rejection::new(code, recheck_reason(&classified.disposition))
}

fn recheck_reason(disposition: &PruneDisposition) -> String {
    match disposition {
        PruneDisposition::Removable => String::new(),
        PruneDisposition::SkippedProtected(reason) | PruneDisposition::SkippedIssue(reason) => {
            reason.clone()
        }
        PruneDisposition::MissingDirectory => "the directory vanished".to_owned(),
    }
}

fn post_move_recheck(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    candidate: &DirectoryCandidate,
) -> Result<Option<crate::worktree::cache::CacheInventory>, Rejection> {
    let fresh = registry::load_records(storage, Some(scope.identity)).map_err(|error| {
        Rejection::new(
            "registry_read_failed",
            format!(
                "could not re-read the directory registry: {}",
                error.message
            ),
        )
    })?;
    let fresh_scope = PruneScope {
        identity: scope.identity,
        repo_path: scope.repo_path,
        records: &fresh,
        target: scope.target,
    };
    let fresh_record = fresh
        .iter()
        .find(|record| record.directory_id == candidate.directory_id)
        .ok_or_else(|| {
            Rejection::new(
                "registry_record_missing",
                "the candidate's registry record is gone after the moves",
            )
        })?;
    source.invalidate(scope.repo_path, &fresh_record.issue_numbers());
    let classified = classify(
        runner,
        api,
        &fresh_scope,
        source,
        fresh_record,
        &std::cell::OnceCell::new(),
    );
    match &classified.disposition {
        PruneDisposition::Removable => {}
        _ => return Err(classification_rejection(&classified)),
    }
    let associated = associated_sessions(&classified);
    if let FamilyOutcome::Keep(reason) = sessions::finalize_family(
        api,
        CleanupMode::Verify,
        scope.target,
        &candidate.canonical_path,
        &associated,
        None,
    ) {
        return Err(Rejection::new("session_family", reason));
    }
    Ok(classified.cache)
}

#[allow(clippy::too_many_arguments)]
fn apply_removal(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    scope: &PruneScope<'_>,
    candidate: &DirectoryCandidate,
    action: &mut PruneAction,
    timing: &TimingStats,
    removed_cache: Option<usize>,
) {
    let worktree_path = PathBuf::from(&candidate.worktree_path);
    // The removal belongs to the reclamation stage: the Git worktree removal
    // and the registry row removal are measured with the cache clearance,
    // including the calls that fail.
    let removal = timing.measure_stage(timing::CACHE_DELETE_RECLAIM, || {
        worktree_remove(runner, scope.repo_path, &worktree_path)
    });
    match removal {
        Ok(()) => {
            let cleanup = timing.measure_stage(timing::CACHE_DELETE_RECLAIM, || {
                registry::remove_directory(storage, scope.identity, &candidate.directory_id)
            });
            match cleanup {
                Ok(_) => {
                    action.prunable = true;
                    action.result = "pruned".to_owned();
                    action.message = Some(match removed_cache {
                        Some(count) => format!(
                            "removed cache-only .cache ({count} entries); git worktree remove succeeded"
                        ),
                        None => "git worktree remove succeeded".to_owned(),
                    });
                }
                Err(error) => {
                    action.prunable = true;
                    action.result = "failed".to_owned();
                    action.reason_code = Some("registry_cleanup_failed");
                    action.message = Some(format!(
                        "worktree removed but registry cleanup failed: {}",
                        error.message
                    ));
                }
            }
        }
        Err(error) => {
            report_local_warnings(
                "worktree prune",
                Some(format!(
                    "git worktree remove failed for {}: {}",
                    worktree_path.display(),
                    error.message
                )),
            );
            action.result = "failed".to_owned();
            action.reason_code = Some("worktree_remove_failed");
            action.message = Some(format!("worktree_remove error: {}", error.message));
        }
    }
}

fn summarize(actions: Vec<PruneAction>, scanned: usize, mode: PruneMode) -> PruneSummary {
    let mut summary = PruneSummary {
        scanned,
        removed: 0,
        eligible: 0,
        absent: 0,
        kept_protected: 0,
        kept_issue: 0,
        failed: 0,
        dry_run: mode.is_report(),
        actions: Vec::with_capacity(actions.len()),
    };
    for action in actions {
        match classify_outcome(&action) {
            PruneOutcome::Pruned => summary.removed += 1,
            PruneOutcome::Prunable => summary.eligible += 1,
            PruneOutcome::Absent => summary.absent += 1,
            PruneOutcome::KeptProtected => summary.kept_protected += 1,
            PruneOutcome::KeptIssue => summary.kept_issue += 1,
            PruneOutcome::Failed => summary.failed += 1,
            PruneOutcome::Noop => {}
        }
        summary.actions.push(action);
    }
    summary
}

#[cfg(test)]
#[path = "worktrees/tests.rs"]
mod tests;
