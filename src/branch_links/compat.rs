//! Compatible single-issue resolution over durable links.
//!
//! Status, commit-hook, and desktop surfaces expose one `issue_id` for
//! adapter callers. It is populated only when unambiguous and the branch
//! is not the detected default: multiple distinct linked issues are never
//! guessed (the worktree lease disambiguates instead), and the detected
//! default branch never reports an active issue. When no durable row
//! resolves, the branch name itself is the fallback (`<type>/<id>`; see
//! [`crate::branch_naming`]) and a protected branch name never resolves.
//!
//! Identity is `(provider, project, issue_number)`: two rows that share
//! a number under different scopes are distinct identities (issue 628)
//! and resolve ambiguous, never single. The numeric
//! [`resolve_compat_issue`] projection treats each number as one
//! scope-unknown identity, which reproduces the historical behavior
//! for single-scope callers; scoped callers use
//! [`resolve_scoped_compat_issue`] so cross-scope duplicates stay
//! ambiguous there too.

#![allow(dead_code)]

use crate::branch_links::identity::is_protected_branch;
use crate::branch_links::reads::LinkedIssue;
use crate::branch_links::store::STATUS_LINKED;
use crate::branch_links::{
    UnknownState, checkout_root, ensure_schema, issues_for_branch, read_origin_url,
    resolve_repo_key,
};
use crate::branch_naming::parse_issue_id_from_branch_name;
use crate::git_runner::GitRunner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatIssue {
    pub issue_number: Option<u64>,
    pub ambiguous: bool,
    pub suppressed_default: bool,
}

/// One linked row as seen by [`resolve_scoped_compat_issue`]: the
/// provider/project scope plus the numeric issue id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopedIssueRef<'a> {
    pub provider: &'a str,
    pub project: &'a str,
    pub issue_number: u64,
}

/// Resolve the compatible issue number from linked issue numbers.
/// `linked` are the distinct `issue_number` values of the branch's active
/// link rows (already scope-filtered by the caller, or all scopes when
/// unscoped).
pub fn resolve_compat_issue(
    linked: &[u64],
    branch: &str,
    default_branch: Option<&str>,
) -> CompatIssue {
    // Numeric projection: every number is one scope-unknown identity,
    // so single-scope callers keep their historical answers exactly.
    let mut numbers = linked.to_vec();
    numbers.sort_unstable();
    numbers.dedup();
    let linked_ids: Vec<(String, String, u64)> = numbers
        .into_iter()
        .map(|number| (String::new(), String::new(), number))
        .collect();
    resolve_core(&linked_ids, branch, default_branch)
}

/// Resolve the compatible issue number from scope-qualified link rows.
/// Two rows that share a number under different `(provider, project)`
/// scopes are distinct identities and resolve ambiguous instead of
/// collapsing to that number; default-branch suppression matches
/// [`resolve_compat_issue`].
pub fn resolve_scoped_compat_issue(
    linked: &[ScopedIssueRef<'_>],
    branch: &str,
    default_branch: Option<&str>,
) -> CompatIssue {
    let mut linked_ids: Vec<(String, String, u64)> = linked
        .iter()
        .map(|entry| {
            (
                entry.provider.to_owned(),
                entry.project.to_owned(),
                entry.issue_number,
            )
        })
        .collect();
    linked_ids.sort_unstable();
    linked_ids.dedup();
    resolve_core(&linked_ids, branch, default_branch)
}

/// Durable-link resolution with the branch-name fallback: the active
/// linked identity when exactly one resolves, otherwise the `<type>/<id>`
/// name, otherwise `None`. Ambiguity and the detected default suppress
/// both sources; a protected branch name never resolves.
pub fn resolve_branch_issue(
    linked: &[ScopedIssueRef<'_>],
    branch: &str,
    default_branch: Option<&str>,
) -> Option<u64> {
    let compat = resolve_scoped_compat_issue(linked, branch, default_branch);
    if compat.ambiguous || compat.suppressed_default {
        return None;
    }
    compat
        .issue_number
        .or_else(|| named_branch_issue(branch, default_branch))
}

/// Branch-name fallback: the issue id encoded in the branch name. A
/// protected name (the detected default, or a conventional `main`/`master`
/// while detection is unknown) never resolves.
pub fn named_branch_issue(branch: &str, default_branch: Option<&str>) -> Option<u64> {
    if is_protected_branch(branch, default_branch) {
        return None;
    }
    parse_issue_id_from_branch_name(branch)
}

/// Durable + named resolution with the strict default-branch protection:
/// a detected default, or a conventional `main`/`master` while detection
/// is unknown, resolves nothing from either source.
pub fn resolve_protected_branch_issue(
    linked: &[ScopedIssueRef<'_>],
    branch: &str,
    default_branch: Option<&str>,
) -> Option<u64> {
    if is_protected_branch(branch, default_branch) {
        return None;
    }
    resolve_branch_issue(linked, branch, default_branch)
}

/// The active (`linked`) identities among `rows`; detached history is
/// ignored so an explicit unbind is never resurrected.
pub fn linked_refs(rows: &[LinkedIssue]) -> Vec<ScopedIssueRef<'_>> {
    rows.iter()
        .filter(|entry| entry.status == STATUS_LINKED)
        .map(|entry| ScopedIssueRef {
            provider: &entry.issue.provider,
            project: &entry.issue.project,
            issue_number: entry.issue_number,
        })
        .collect()
}

/// Read this checkout's link rows (linked and detached) for `branch` from
/// the local store, or `None` when the store cannot answer. Never touches
/// the network.
pub fn read_branch_link_rows(runner: &dyn GitRunner, branch: &str) -> Option<Vec<LinkedIssue>> {
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

/// Resolve the active issue for `branch` from its durable link rows, with
/// the branch-name fallback and the strict default-branch protection. A
/// missing store degrades to the branch name.
pub fn resolve_branch_rows(
    rows: Option<&[LinkedIssue]>,
    branch: &str,
    default_branch: Option<&str>,
) -> Option<u64> {
    match rows {
        Some(rows) => {
            let linked = linked_refs(rows);
            resolve_protected_branch_issue(&linked, branch, default_branch)
        }
        None => named_branch_issue(branch, default_branch),
    }
}

/// Shared resolution over distinct `(provider, project, issue_number)`
/// identities: more than one linked identity is never guessed, and the
/// detected default branch never reports an active issue.
fn resolve_core(
    linked_ids: &[(String, String, u64)],
    branch: &str,
    default_branch: Option<&str>,
) -> CompatIssue {
    let distinct: Vec<u64> = {
        let mut numbers: Vec<u64> = linked_ids.iter().map(|(_, _, number)| *number).collect();
        numbers.sort_unstable();
        numbers.dedup();
        numbers
    };
    if default_branch.is_some_and(|default| default == branch) {
        return CompatIssue {
            issue_number: None,
            ambiguous: distinct.len() > 1 || linked_ids.len() > 1,
            suppressed_default: true,
        };
    }
    if linked_ids.len() > 1 {
        return CompatIssue {
            issue_number: None,
            ambiguous: true,
            suppressed_default: false,
        };
    }
    match distinct.len() {
        1 => CompatIssue {
            issue_number: Some(distinct[0]),
            ambiguous: false,
            suppressed_default: false,
        },
        0 => CompatIssue {
            issue_number: None,
            ambiguous: false,
            suppressed_default: false,
        },
        _ => CompatIssue {
            issue_number: None,
            ambiguous: true,
            suppressed_default: false,
        },
    }
}
