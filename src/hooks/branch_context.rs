//! Database-backed issue resolution for commit hooks (issue 628 P3,
//! hardened in P4).
//!
//! Durable links are authoritative: the current branch's linked issue
//! identities decide, with the legacy Git binding as fallback only when
//! the database holds nothing for the branch. Exactly one distinct
//! `(provider, project, issue_number)` identity wins; several are never
//! guessed — including rows that share a number across scopes. The
//! detected default branch never resolves (cached-only detection;
//! unknown detection keeps legacy behavior), and detached HEAD stays
//! silent like before. Any store failure degrades to the legacy Git
//! read so hooks never fail a commit over the database.

use crate::branch_context::{BranchContextError, GitRunner};
use crate::branch_links::compat::{ScopedIssueRef, resolve_scoped_compat_issue};
use crate::branch_links::{
    LinkedIssue, UnknownState, checkout_root, detect_default_branch, ensure_schema,
    issues_for_branch, read_origin_url, resolve_repo_key,
};

/// Resolve the single compatible issue id for `runner`'s current
/// branch, or `Ok(None)` when no active issue applies.
pub fn resolve_hook_issue_id(runner: &dyn GitRunner) -> Result<Option<u64>, BranchContextError> {
    let branch = match crate::branch_context::current_branch(runner) {
        Ok(branch) => branch,
        Err(error) if error.kind == "branch" => return Ok(None),
        Err(error) => return Err(error),
    };
    let legacy = crate::branch_context::read_issue_id(runner, &branch)?;
    // The detected default branch is never an active issue branch.
    // A conventional default name is refused while detection is
    // unknown (cached-only detection; an undetected `main` is still
    // `main`): the durable-link guess below is skipped and the
    // explicit legacy binding decides, as before. Every other branch
    // keeps legacy behavior on unknown detection.
    let default = detect_default_branch(runner);
    if default.as_deref() == Some(branch.as_str()) {
        return Ok(None);
    }
    let conventional_unknown =
        default.is_none() && crate::branch_links::identity::is_protected_branch(&branch, None);
    let rows = match link_entries(runner, &branch) {
        Some(rows) if !conventional_unknown => rows,
        // No durable signal, or a conventional default with no cached
        // HEAD to disprove it: legacy Git binding decides, as before.
        _ => return Ok(legacy),
    };
    let mut linked: Vec<ScopedIssueRef> = Vec::new();
    let mut detached: Vec<ScopedIssueRef> = Vec::new();
    for entry in &rows {
        let scoped = ScopedIssueRef {
            provider: &entry.issue.provider,
            project: &entry.issue.project,
            issue_number: entry.issue_number,
        };
        if entry.status == crate::branch_links::store::STATUS_LINKED {
            linked.push(scoped);
        } else {
            detached.push(scoped);
        }
    }
    let compat = resolve_scoped_compat_issue(&linked, &detached, legacy, &branch, None);
    Ok(compat.issue_number)
}

/// Linked and detached link rows for `branch`, or `None` when the
/// store cannot answer (missing database, missing table, unreadable
/// origin) so the caller falls back to the legacy read.
fn link_entries(runner: &dyn GitRunner, branch: &str) -> Option<Vec<LinkedIssue>> {
    let repo_key =
        resolve_repo_key(read_origin_url(runner).as_deref(), &checkout_root(runner)).ok()?;
    let storage = crate::infra::storage::Storage::open().ok()?;
    ensure_schema(&storage.connection).ok()?;
    issues_for_branch(
        &storage.connection,
        &repo_key.key,
        branch,
        true,
        &UnknownState,
    )
    .ok()
}
