//! Durable branch-link resolution for commit hooks.
//!
//! Active durable links are authoritative: the current branch's linked
//! issue identities decide, with the branch name (`<type>/<id>`) as the
//! fallback. Exactly one distinct `(provider, project, issue_number)`
//! identity wins; several are never guessed — including rows that share a
//! number across scopes. The detected default branch never resolves, and a
//! conventional default name is refused while detection is unknown
//! (cached-only detection; an undetected `main` is still `main`). Detached
//! HEAD stays silent. Any store failure degrades to the branch-name read so
//! hooks never fail a commit over the database.

use crate::branch_links::compat::{read_branch_link_rows, resolve_branch_rows};
use crate::branch_links::detect_default_branch;
use crate::git_runner::{GitError, GitRunner};

/// Resolve the single compatible issue id for `runner`'s current
/// branch, or `Ok(None)` when no active issue applies.
pub fn resolve_hook_issue_id(runner: &dyn GitRunner) -> Result<Option<u64>, GitError> {
    let branch = match crate::git_runner::current_branch(runner) {
        Ok(branch) => branch,
        Err(error) if error.kind == "branch" => return Ok(None),
        Err(error) => return Err(error),
    };
    // Cached-only detection, never a network query; the resolver refuses
    // the detected default and a conventional default name while detection
    // is unknown.
    let default = detect_default_branch(runner);
    let rows = read_branch_link_rows(runner, &branch);
    Ok(resolve_branch_rows(
        rows.as_deref(),
        &branch,
        default.as_deref(),
    ))
}
