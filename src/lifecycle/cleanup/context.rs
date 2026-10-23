//! What one cleanup pass needs before it may touch a directory: the
//! repository identity, and every lease row recorded against it.
//!
//! Establishing this is the only part of the cleanup that is allowed to
//! fail outright, and when it does the pass removes nothing. That is the
//! pre-existing shape of the helper, kept intact: a remote close has
//! already succeeded, so an unreadable identity or lease store degrades to
//! one bounded warning rather than an error.

use std::path::Path;

use crate::infra::storage::Storage;
use crate::worktree::{LeaseRow, WorktreeRunner};

/// The repository identity plus every lease row recorded against it.
pub(crate) struct CleanupContext {
    pub(crate) identity: String,
    pub(crate) rows: Vec<LeaseRow>,
}

impl CleanupContext {
    /// True when the issue still has a row in this repository. An issue
    /// with no row has no candidate at all, and the pass is a no-op.
    pub(super) fn has_issue(&self, issue: u64) -> bool {
        self.rows.iter().any(|row| row.issue == issue)
    }

    /// Load the current rows for an already-known repository identity.
    /// A caller that owns the lease store reuses it instead of reopening
    /// the database, and the read is always fresh: the cleanup guards must
    /// see the rows as they are after any release, not a scan snapshot.
    pub(crate) fn load(storage: &Storage, identity: &str) -> Result<Self, String> {
        match crate::worktree::leases::list_all_for_repo(storage, identity) {
            Ok(rows) => Ok(Self {
                identity: identity.to_owned(),
                rows,
            }),
            Err(error) => Err(error.message),
        }
    }
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
    match crate::worktree::leases::list_for_repo(&storage, &identity) {
        Ok(rows) => Ok(CleanupContext { identity, rows }),
        Err(error) => Err(warning(
            issue,
            "could not read worktree leases for cleanup",
            &error.message,
        )),
    }
}

fn warning(issue: u64, what: &str, detail: &str) -> String {
    format!(
        "issue {issue} closed; {what}: {}",
        crate::lifecycle::bounded(detail)
    )
}
