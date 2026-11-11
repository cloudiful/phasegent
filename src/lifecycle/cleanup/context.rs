//! What one cleanup pass needs before it may touch a directory: the
//! repository identity and every registered directory recorded against it.
//!
//! Establishing this is the only part of the cleanup that is allowed to fail
//! outright, and when it does the pass removes nothing. A remote close has
//! already succeeded, so an unreadable identity or registry degrades to one
//! bounded warning rather than an error.

use std::path::Path;

use crate::infra::storage::Storage;
use crate::worktree::WorktreeRunner;
use crate::worktree::registry::{self, DirectoryRecord};

/// The repository identity plus every registered directory recorded against
/// it.
pub(crate) struct CleanupContext {
    pub(crate) identity: String,
    pub(crate) records: Vec<DirectoryRecord>,
}

impl CleanupContext {
    /// True when the issue still has a registered directory in this
    /// repository. An issue with no association has no candidate at all, and
    /// the pass is a no-op.
    pub(super) fn has_issue(&self, issue: u64) -> bool {
        self.records.iter().any(|record| record.has_issue(issue))
    }

    #[cfg(test)]
    pub(super) fn has_scoped_directory(&self, directory: &Path) -> bool {
        self.records
            .iter()
            .filter(|record| super::same_directory(record.canonical_path().as_path(), directory))
            .any(|record| record.is_scoped())
    }

    /// Whether the directory may be treated by a provider-scoped cleanup
    /// pass. An unscoped legacy directory is always allowed. A directory with
    /// any scoped association is allowed only when the pass names its allowed
    /// provider/project and every association carries that same provider and
    /// project; a task-scoped pass is stricter still and requires the same
    /// durable task. Mixed or foreign directories keep.
    pub(crate) fn directory_scope_allowed(
        &self,
        directory: &Path,
        allowed_scope: Option<&crate::worktree::WorktreeScope>,
    ) -> bool {
        let Some(record) = self
            .records
            .iter()
            .find(|record| super::same_directory(record.canonical_path().as_path(), directory))
        else {
            return true;
        };
        if !record.is_scoped() {
            return true;
        }
        let Some(allowed_scope) = allowed_scope else {
            return false;
        };
        if record.tasks.iter().any(|task| {
            task.provider.as_deref() != Some(allowed_scope.provider())
                || task.project.as_deref() != Some(allowed_scope.project())
                || allowed_scope
                    .task_id()
                    .is_some_and(|task_id| task.task_id != task_id)
        }) {
            return false;
        }
        if record.legacy_issues.iter().any(|issue| {
            (!issue.provider.is_empty() || !issue.project.is_empty())
                && (issue.provider != allowed_scope.provider()
                    || issue.project != allowed_scope.project())
        }) {
            return false;
        }
        true
    }

    /// Load the current records for an already-known repository identity. The
    /// read is always fresh: the cleanup guards must see the registry as it is
    /// after any earlier mutation, not a scan snapshot.
    pub(crate) fn load(storage: &Storage, identity: &str) -> Result<Self, String> {
        match load_repository(storage, identity) {
            Ok(records) => Ok(Self {
                identity: identity.to_owned(),
                records,
            }),
            Err(error) => Err(error),
        }
    }
}

/// Every registered directory recorded against `identity`, uncapped.
pub(crate) fn load_repository(
    storage: &Storage,
    identity: &str,
) -> Result<Vec<DirectoryRecord>, String> {
    registry::load_records(storage, Some(identity)).map_err(|error| error.message)
}

/// Resolve the pass's context, or the bounded reason it could not be
/// established.
/// Resolve the context together with the store it was read from, so a
/// destructive pass can reload the same store after taking its operation lock.
pub(super) fn resolve_with_storage(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    issue: u64,
) -> Result<(Storage, CleanupContext), String> {
    if issue == 0 {
        return Err(warning(
            issue,
            "cannot resolve worktree cleanup context",
            "issue number is zero",
        ));
    }
    let identity = match crate::worktree::repo_identity(runner, repo_path) {
        Ok(identity) => identity,
        Err(error) => {
            return Err(warning(
                issue,
                "could not resolve repository identity for worktree cleanup",
                &error.message,
            ));
        }
    };
    let storage = match crate::infra::storage::Storage::open() {
        Ok(storage) => storage,
        Err(error) => {
            return Err(warning(
                issue,
                "could not open the directory registry for worktree cleanup",
                &error,
            ));
        }
    };
    if let Err(error) = crate::worktree::ensure_schema(&storage) {
        return Err(warning(
            issue,
            "could not initialise the directory registry for worktree cleanup",
            &error,
        ));
    }
    match CleanupContext::load(&storage, &identity) {
        Ok(context) => Ok((storage, context)),
        Err(error) => Err(warning(
            issue,
            "could not read worktree directories for cleanup",
            &error,
        )),
    }
}

fn warning(issue: u64, what: &str, detail: &str) -> String {
    format!(
        "issue {issue} closed; {what}: {}",
        crate::lifecycle::bounded(detail)
    )
}
