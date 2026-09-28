//! Repo-wide active-lease conflict policy for safe switching.
//!
//! Split from `issue_switch.rs` so the switch gates live in one cohesive
//! module. An active lease on this checkout owned by another
//! `(issue, session)` conflicts, and so does any other live lease in the
//! repository: switching the primary checkout preserves the repo-wide
//! active-lease policy, not merely the current path.

use crate::infra::storage::Storage;
use crate::lifecycle::IssueSwitchParams;
use crate::worktree::{LEASE_STATUS_ACTIVE, WorktreeRunner, list_for_repo, repo_identity};

/// An active lease on this checkout owned by another `(issue, session)`
/// conflicts with switching it, and so does any other live lease in the
/// repository. Our own triple re-entering is idempotent, and terminal
/// history never conflicts. Storage or identity failures fail closed:
/// without verification the checkout stays untouched.
pub(super) fn lease_conflict(
    storage: &Storage,
    wt: &dyn WorktreeRunner,
    params: &IssueSwitchParams<'_>,
    issue_id: u64,
    branch: &str,
    current: &str,
) -> Option<String> {
    let identity = match repo_identity(wt, params.repo_path) {
        Ok(identity) => identity,
        Err(error) => {
            return Some(format!(
                "linked '{branch}' but could not verify active leases ({}); leaving \
                 '{current}' untouched",
                error.message
            ));
        }
    };
    let rows = match list_for_repo(storage, &identity) {
        Ok(rows) => rows,
        Err(error) => {
            return Some(format!(
                "linked '{branch}' but could not read active leases ({}); leaving \
                 '{current}' untouched",
                error.message
            ));
        }
    };
    // Ours means our exact `(issue, session)` triple; without a
    // session nothing can prove ownership, so every other live row
    // counts as foreign. Mirrors the acquire Rule 4 scan.
    let is_ours = |session: &str, issue: u64| {
        issue == issue_id && params.session.is_some_and(|ours| ours == session)
    };
    let here = params.repo_path.to_string_lossy();
    if let Some(row) = rows.iter().find(|row| {
        row.status == LEASE_STATUS_ACTIVE
            && row.worktree_path == here
            && !is_ours(&row.session, row.issue)
    }) {
        return Some(format!(
            "linked '{branch}' but session '{}' holds this checkout for issue {} \
             (lease {}); leaving '{current}' untouched — isolate with \
             `phasegent worktree acquire --issue {issue_id} --isolate`",
            row.session, row.issue, row.lease_id
        ));
    }
    if let Some(row) = rows
        .iter()
        .find(|row| row.status == LEASE_STATUS_ACTIVE && !is_ours(&row.session, row.issue))
    {
        return Some(format!(
            "linked '{branch}' but session '{}' holds an active lease for issue {} on '{}' \
             (lease {}) in this repository; leaving '{current}' untouched — coordinate with \
             that session, or isolate with \
             `phasegent worktree acquire --issue {issue_id} --isolate`",
            row.session, row.issue, row.worktree_path, row.lease_id
        ));
    }
    None
}
