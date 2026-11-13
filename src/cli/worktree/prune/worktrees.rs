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
        .filter(|record| !same_path(Path::new(&record.worktree_path), scope.repo_path))
        .map(|record| {
            if absent_only {
                let mut candidate = classify_absent_only(record);
                match path_absent(Path::new(&record.worktree_path)) {
                    Ok(true) => {}
                    Ok(false) => {
                        candidate.reason_code = Some("repository_unavailable");
                        candidate.disposition = PruneDisposition::SkippedProtected(
                            "repository checkout is unavailable; existing directory retained"
                                .to_owned(),
                        );
                    }
                    Err(reason) => {
                        candidate.reason_code = Some("path_probe_error");
                        candidate.disposition = PruneDisposition::SkippedProtected(reason);
                    }
                }
                candidate
            } else {
                classify(runner, api, scope, source, record, snapshot)
            }
        })
        .collect();
    candidates.sort_by(|a, b| a.canonical_path.cmp(&b.canonical_path));
    candidates
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
    // One per-repository operation lock covers the whole removal pass: fresh
    // checks, Git mutation, and the short registry transactions share it.
    // A preview and every other read path create no lock file.
    let _lock = if mode.is_report() {
        None
    } else {
        match crate::worktree::operation_lock::acquire(scope.identity) {
            Ok(lock) => Some(lock),
            Err(error) => {
                let actions = scope
                    .records
                    .iter()
                    .filter(|record| !same_path(Path::new(&record.worktree_path), scope.repo_path))
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
    let candidates = scan_with_paths(runner, scan_api, scope, source, snapshot, absent_only);
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
                let check = registry::load_records(storage, Some(scope.identity))
                    .map_err(|_| "could not re-read the directory registry".to_owned())
                    .and_then(|records| {
                        if !records.iter().any(|record| record == &candidate.expected) {
                            return Err("directory registration changed after the scan".to_owned());
                        }
                        if !path_absent(Path::new(&candidate.worktree_path))? {
                            return Err("directory path reappeared after the scan".to_owned());
                        }
                        registry::remove_directory(storage, scope.identity, &candidate.directory_id)
                            .map_err(|_| "directory registration cleanup failed".to_owned())
                            .and_then(|removed| {
                                if removed == 1 {
                                    Ok(())
                                } else {
                                    Err("directory registration disappeared".to_owned())
                                }
                            })
                    });
                match check {
                    Ok(()) => {
                        action.message = Some("absent directory registration reclaimed".to_owned())
                    }
                    Err(reason) => {
                        action.result = "skipped_protected".to_owned();
                        action.message = Some(reason);
                    }
                }
            }
        }
        PruneDisposition::Removable => {
            if mode.is_report() {
                match preview_family(api, scope, candidate) {
                    FamilyOutcome::Keep(reason) => {
                        action.result = "skipped_protected".to_owned();
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
                match recheck(storage, runner, api, scope, source, candidate) {
                    Err(reason) => {
                        action.result = "skipped_protected".to_owned();
                        action.message = Some(format!("kept on re-check: {reason}"));
                    }
                    Ok((associated, fresh_cache)) => {
                        if fresh_cache != candidate.cache {
                            action.result = "skipped_protected".to_owned();
                            action.message = Some(
                                "kept on re-check: the root .cache inventory changed before the moves"
                                    .to_owned(),
                            );
                        } else {
                            match sessions::finalize_family_with(
                                api,
                                CleanupMode::Prune,
                                scope.target,
                                &candidate.canonical_path,
                                &associated,
                                None,
                                confirmation,
                            ) {
                                FamilyOutcome::Keep(reason) => {
                                    action.result = "skipped_protected".to_owned();
                                    action.message = Some(format!("kept on re-check: {reason}"));
                                }
                                _ => match post_move_recheck(
                                    storage, runner, api, scope, source, candidate,
                                ) {
                                    Err(reason) => {
                                        action.result = "skipped_protected".to_owned();
                                        action.message =
                                            Some(format!("kept on re-check: {reason}"));
                                    }
                                    Ok(_) => {
                                        stages(PruneStage::Deleting {
                                            candidate: candidate_index,
                                        });
                                        apply_removal(
                                            storage,
                                            runner,
                                            api,
                                            scope,
                                            source,
                                            candidate,
                                            &mut action,
                                        );
                                    }
                                },
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
) -> Result<(Vec<String>, Option<crate::worktree::cache::CacheInventory>), String> {
    let fresh = registry::load_records(storage, Some(scope.identity)).map_err(|error| {
        format!(
            "could not re-read the directory registry: {}",
            error.message
        )
    })?;
    if canonical_or_self(Path::new(&candidate.worktree_path)) != candidate.canonical_path {
        return Err("the directory path changed its target".to_owned());
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
        .ok_or_else(|| "the candidate's registry record is gone".to_owned())?;
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
        Err(recheck_reason(classified.disposition))
    }
}

fn recheck_reason(disposition: PruneDisposition) -> String {
    match disposition {
        PruneDisposition::Removable => String::new(),
        PruneDisposition::SkippedProtected(reason) | PruneDisposition::SkippedIssue(reason) => {
            reason
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
) -> Result<Option<crate::worktree::cache::CacheInventory>, String> {
    let fresh = registry::load_records(storage, Some(scope.identity)).map_err(|error| {
        format!(
            "could not re-read the directory registry: {}",
            error.message
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
        .ok_or_else(|| "the candidate's registry record is gone after the moves".to_owned())?;
    source.invalidate(scope.repo_path, &fresh_record.issue_numbers());
    let classified = classify(
        runner,
        api,
        &fresh_scope,
        source,
        fresh_record,
        &std::cell::OnceCell::new(),
    );
    match classified.disposition {
        PruneDisposition::Removable => {}
        other => return Err(recheck_reason(other)),
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
        return Err(reason);
    }
    Ok(classified.cache)
}

fn apply_removal(
    storage: &Storage,
    runner: &dyn WorktreeRunner,
    api: &dyn OpenCodeApi,
    scope: &PruneScope<'_>,
    source: &dyn IssueStateSource,
    candidate: &DirectoryCandidate,
    action: &mut PruneAction,
) {
    let worktree_path = PathBuf::from(&candidate.worktree_path);
    let removed_cache = match crate::worktree::cache::clear_cache_only(
        runner,
        &worktree_path,
        candidate.cache.as_ref(),
    ) {
        Ok(count) => count,
        Err(reason) => {
            action.result = "skipped_protected".to_owned();
            action.message = Some(format!("kept on re-check: {reason}"));
            return;
        }
    };
    // Clearing the cache is a bounded mutation window. Re-read the registry,
    // closure evidence, local guards and fresh host occupancy before the Git
    // removal; a directory that gained an owner, reopened, became dirty, or
    // regained a root .cache keeps the tree.
    match post_move_recheck(storage, runner, api, scope, source, candidate) {
        Err(reason) => {
            action.result = "skipped_protected".to_owned();
            action.message = Some(format!("kept after cache cleanup: {reason}"));
            return;
        }
        Ok(Some(_)) => {
            action.result = "skipped_protected".to_owned();
            action.message = Some(
                "kept after cache cleanup: a root .cache reappeared before the worktree removal"
                    .to_owned(),
            );
            return;
        }
        Ok(None) => {}
    }
    match worktree_remove(runner, scope.repo_path, &worktree_path) {
        Ok(()) => {
            match registry::remove_directory(storage, scope.identity, &candidate.directory_id) {
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
