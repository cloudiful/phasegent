//! What one cleanup pass needs before it may touch a directory: the
//! repository identity, and every lease row recorded against it.
//!
//! Establishing this is the only part of the cleanup that is allowed to
//! fail outright, and when it does the pass removes nothing. That is the
//! pre-existing shape of the helper, kept intact: a remote close has
//! already succeeded, so an unreadable identity or lease store degrades to
//! one bounded warning rather than an error.
//!
//! The lease read is the uncapped repository read: a directory whose only
//! active reference or open-issue link sits beyond a row ceiling must still
//! protect that directory, so incomplete evidence is never a permission.

use std::path::Path;

use crate::infra::storage::Storage;
use crate::worktree::{LeaseRow, WorktreeRunner};

/// The repository identity plus every lease row recorded against it.
pub(crate) struct CleanupContext {
    pub(crate) identity: String,
    pub(crate) rows: Vec<LeaseRow>,
    pub(crate) scopes: std::collections::HashMap<String, crate::worktree::LeaseScope>,
}

impl CleanupContext {
    /// True when the issue still has a row in this repository. An issue
    /// with no row has no candidate at all, and the pass is a no-op.
    pub(super) fn has_issue(&self, issue: u64) -> bool {
        self.rows.iter().any(|row| row.issue == issue)
    }

    #[cfg(test)]
    pub(super) fn has_scoped_directory(&self, directory: &Path) -> bool {
        self.rows.iter().any(|row| {
            self.scopes.contains_key(&row.lease_id)
                && super::same_directory(Path::new(&row.worktree_path), directory)
        })
    }

    /// Whether the directory may be treated by a provider-scoped cleanup
    /// pass. An unscoped legacy directory is always allowed (the legacy
    /// path is unchanged). A directory with any scoped lease is allowed
    /// only when the pass names its allowed provider/project and every lease
    /// on the directory — scoped or not — carries that same provider and
    /// project; mixed or foreign directories keep. Individual release and
    /// candidate selection still use the exact per-issue key.
    ///
    /// A task-scoped pass is stricter still: every scoped lease on the
    /// directory must carry the same durable task, so a same-number task on
    /// another instance never shares a directory with the closed task.
    pub(super) fn directory_scope_allowed(
        &self,
        directory: &Path,
        allowed_scope: Option<&crate::worktree::LeaseScope>,
    ) -> bool {
        let rows: Vec<&LeaseRow> = self
            .rows
            .iter()
            .filter(|row| super::same_directory(Path::new(&row.worktree_path), directory))
            .collect();
        if !rows
            .iter()
            .any(|row| self.scopes.contains_key(&row.lease_id))
        {
            return true;
        }
        let Some(allowed_scope) = allowed_scope else {
            return false;
        };
        rows.iter().all(|row| {
            self.scopes.get(&row.lease_id).is_some_and(|scope| {
                if scope.provider() != allowed_scope.provider()
                    || scope.project() != allowed_scope.project()
                {
                    return false;
                }
                match allowed_scope.task_id() {
                    // A task-scoped pass only owns the leases of that exact
                    // task; a legacy or other-task row keeps the directory.
                    Some(task_id) => scope.task_id() == Some(task_id),
                    None => true,
                }
            })
        })
    }

    /// Load the current rows for an already-known repository identity.
    /// A caller that owns the lease store reuses it instead of reopening
    /// the database, and the read is always fresh: the cleanup guards must
    /// see the rows as they are after any release, not a scan snapshot.
    ///
    /// The sidecar map is checked against the durable key of the physical
    /// repository the identity names, so a task-bearing lease row that is
    /// not a member of that repository is refused here and the whole pass
    /// keeps its directories.
    pub(crate) fn load(storage: &Storage, identity: &str) -> Result<Self, String> {
        let repo_key = crate::worktree::lease_scope::verified_repo_key_for_identity(identity);
        match load_repository(storage, identity) {
            Ok(rows) => Ok(Self {
                identity: identity.to_owned(),
                rows,
                scopes: crate::worktree::lease_scope::scope_map_for_repo_with_key(
                    storage,
                    identity,
                    repo_key.as_deref(),
                )
                .map_err(|error| error.message)?,
            }),
            Err(error) => Err(error),
        }
    }
}

/// Every lease row recorded against `identity`, uncapped. The lifecycle's
/// own readers call this through the pass's already-open store or a fresh
/// one; an error here is a keep, never a permission.
pub(crate) fn load_repository(storage: &Storage, identity: &str) -> Result<Vec<LeaseRow>, String> {
    crate::worktree::leases::list_all_for_repo(storage, identity).map_err(|error| error.message)
}

/// Resolve the pass's context, or the bounded reason it could not be
/// established.
pub(super) fn resolve(
    runner: &dyn WorktreeRunner,
    repo_path: &Path,
    issue: u64,
) -> Result<CleanupContext, String> {
    if issue == 0 {
        return Ok(CleanupContext {
            identity: String::new(),
            rows: Vec::new(),
            scopes: std::collections::HashMap::new(),
        });
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
                "could not open the lease store for worktree cleanup",
                &error,
            ));
        }
    };
    if let Err(error) = crate::worktree::leases::ensure_schema(&storage) {
        return Err(warning(
            issue,
            "could not initialise the lease store for worktree cleanup",
            &error,
        ));
    }
    match CleanupContext::load(&storage, &identity) {
        Ok(context) => Ok(context),
        Err(error) => Err(warning(
            issue,
            "could not read worktree leases for cleanup",
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
