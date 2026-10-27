//! Durable branch-link resolution for commit hooks.
//!
//! Active durable links are authoritative: the current branch's linked
//! issue identities decide, with the branch name (`<type>/<id>`) as the
//! fallback only when the branch has no active links. Every distinct
//! `(provider, project, issue_number)` identity is a valid commit target, so
//! a branch bound to several issues resolves as one deterministic set. A
//! number that appears under more than one provider/project scope cannot be
//! disambiguated in a commit message, so hooks fail closed instead of
//! silently narrowing validation. The detected default branch never
//! resolves, and a conventional default name is refused while detection is
//! unknown (cached-only detection; an undetected `main` is still `main`).
//! Detached HEAD stays silent. Any store failure degrades to the
//! branch-name read so hooks never fail a commit over the database.

use std::collections::{BTreeMap, BTreeSet};

use crate::branch_links::compat::{linked_refs, read_branch_link_rows};
use crate::branch_links::{detect_default_branch, is_protected_branch, named_branch_issue};
use crate::git_runner::{GitError, GitRunner};

/// The active issue set for the current branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookBindings {
    /// No active binding: hooks stay silent.
    None,
    /// One or more distinct issue numbers, ascending and deduplicated.
    Issues(Vec<u64>),
}

/// Resolve every compatible issue id for `runner`'s current branch.
pub fn resolve_hook_issue_ids(runner: &dyn GitRunner) -> Result<HookBindings, GitError> {
    let branch = match crate::git_runner::current_branch(runner) {
        Ok(branch) => branch,
        Err(error) if error.kind == "branch" => return Ok(HookBindings::None),
        Err(error) => return Err(error),
    };
    // Cached-only detection, never a network query; the detected default
    // and a conventional default name while detection is unknown resolve
    // nothing.
    let default = detect_default_branch(runner);
    if is_protected_branch(&branch, default.as_deref()) {
        return Ok(HookBindings::None);
    }
    match read_branch_link_rows(runner, &branch) {
        Some(rows) => {
            let refs = linked_refs(&rows);
            if refs.is_empty() {
                return Ok(named_bindings(&branch, default.as_deref()));
            }
            let mut scopes: BTreeMap<u64, BTreeSet<(&str, &str)>> = BTreeMap::new();
            for entry in &refs {
                scopes
                    .entry(entry.issue_number)
                    .or_default()
                    .insert((entry.provider, entry.project));
            }
            let ambiguous: Vec<u64> = scopes
                .iter()
                .filter(|(_, found)| found.len() > 1)
                .map(|(number, _)| *number)
                .collect();
            if !ambiguous.is_empty() {
                return Err(GitError::new(
                    "conflict",
                    format!(
                        "issue(s) {ambiguous:?} are linked to this branch under more than one \
                         provider/project scope; refusing to guess which binding a commit belongs \
                         to"
                    ),
                ));
            }
            Ok(HookBindings::Issues(scopes.into_keys().collect()))
        }
        // A missing store degrades to the branch-name fallback.
        None => Ok(named_bindings(&branch, default.as_deref())),
    }
}

fn named_bindings(branch: &str, default: Option<&str>) -> HookBindings {
    match named_branch_issue(branch, default) {
        Some(id) => HookBindings::Issues(vec![id]),
        None => HookBindings::None,
    }
}
