//! Whether a registered directory still protects a different task or issue.
//!
//! Registration alone is never occupancy: a directory whose associated tasks
//! and legacy issues are all confirmed closed is safe to reuse even though its
//! old rows remain. The caller's own exact association is not foreign; every
//! other association, including one with the same number, is evaluated on its
//! full provider/instance/project/task identity. A still-open association keeps
//! the path.
//!
//! Fresh host evidence is part of the same question: a session the registry
//! associates with the directory, or an unassociated occupant the host reports
//! there, keeps it even when every durable task is closed. The seam is
//! deliberately narrow so the worktree layer stays free of provider
//! knowledge: the CLI supplies the closure evidence and the connected host.

use std::path::Path;

use super::opencode::{OpenCodeApi, foreign_occupancy_reason};
use super::registry::{DirectoryLegacyIssue, DirectoryRecord, DirectoryTask, NewAssociation};

/// Closure evidence for a directory's associated tasks and issues.
///
/// Every method answers `true` only for an authoritative closed state. A
/// missing task identity, an unprovable scope, an unavailable provider or any
/// other uncertainty is `false`, which keeps the directory.
pub(crate) trait ForeignIssueState {
    fn task_closed(&self, repo_path: &Path, task: &DirectoryTask) -> bool;

    fn legacy_closed(&self, repo_path: &Path, issue: &DirectoryLegacyIssue) -> bool;
}

/// The bounded reason a directory still protects anything other than the
/// caller's exact association, or `None` when every foreign association is
/// confirmed closed and the fresh host evidence reports no foreign occupant.
///
/// The caller decides what to do with the answer: acquire refuses with
/// isolation guidance, switch leaves the checkout untouched.
pub(crate) fn foreign_protection_reason(
    repo_path: &Path,
    record: &DirectoryRecord,
    own: &NewAssociation,
    state: Option<&dyn ForeignIssueState>,
    api: Option<&dyn OpenCodeApi>,
    own_sessions: &[String],
) -> Option<String> {
    let mut keep: Vec<String> = Vec::new();
    for task in &record.tasks {
        if association_is_ours(own, task) {
            continue;
        }
        if !state.is_some_and(|state| state.task_closed(repo_path, task)) {
            keep.push(format!(
                "task {} for issue {} is not confirmed closed",
                task.task_id, task.issue
            ));
        }
    }
    for issue in &record.legacy_issues {
        if legacy_is_ours(own, issue) {
            continue;
        }
        if !state.is_some_and(|state| state.legacy_closed(repo_path, issue)) {
            keep.push(format!("issue {} is not confirmed closed", issue.issue));
        }
    }
    keep.sort();
    keep.dedup();
    if !keep.is_empty() {
        return Some(keep.join("; "));
    }
    let api = api?;
    foreign_occupancy_reason(
        api,
        &record.canonical_path(),
        own_sessions,
        &record.sessions,
    )
}

/// The reason an unregistered checkout still protects a foreign session.
///
/// A checkout with no registry row is not free occupancy: the connected host
/// is asked about the actual directory and path before the caller reuses or
/// switches it. An unavailable host or an incomplete listing keeps the
/// directory. The caller's own session is exempt only when the host's own
/// identity/lineage evidence proves it belongs to that session's family.
pub(crate) fn unregistered_protection_reason(
    api: Option<&dyn OpenCodeApi>,
    directory: &Path,
    own_sessions: &[String],
) -> Option<String> {
    let api = api?;
    foreign_occupancy_reason(api, &super::canonical_or_self(directory), own_sessions, &[])
}

/// Whether the record already carries exactly the caller's task or legacy
/// identity. Used for idempotent re-entry; a shared number or session alone
/// never qualifies.
pub(crate) fn owns_association(own: &NewAssociation, record: &DirectoryRecord) -> bool {
    record
        .tasks
        .iter()
        .any(|task| association_is_ours(own, task))
        || record
            .legacy_issues
            .iter()
            .any(|issue| legacy_is_ours(own, issue))
}

/// Whether `own` is exactly this durable task or this exact legacy identity.
/// A mere issue number or session is never ownership.
fn association_is_ours(own: &NewAssociation, task: &DirectoryTask) -> bool {
    match own {
        NewAssociation::Task { task_id, .. } => task.task_id == *task_id,
        NewAssociation::Legacy { .. } | NewAssociation::Unscoped { .. } => false,
    }
}

fn legacy_is_ours(own: &NewAssociation, issue: &DirectoryLegacyIssue) -> bool {
    match own {
        NewAssociation::Task { .. } => false,
        NewAssociation::Legacy {
            issue: owned_issue,
            provider,
            project,
            external_id,
        } => {
            issue.issue == *owned_issue
                && issue.provider == *provider
                && issue.project == *project
                && issue.external_id == *external_id
        }
        NewAssociation::Unscoped { issue: owned_issue } => {
            issue.issue == *owned_issue
                && issue.provider.is_empty()
                && issue.project.is_empty()
                && issue.external_id == 0
        }
    }
}
