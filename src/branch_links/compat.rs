//! Compatible single-issue resolution over durable links.
//!
//! Status and commit-hook surfaces expose one `issue_id` for adapter
//! callers. It is populated only when unambiguous and the branch is not
//! the detected default: multiple distinct linked issues are never
//! guessed (the worktree lease disambiguates instead), a legacy Git
//! binding superseded by an explicit detach is ignored, and the detected
//! default branch never reports an active issue.
//!
//! Identity is `(provider, project, issue_number)`: two rows that share
//! a number under different scopes are distinct identities (issue 628)
//! and resolve ambiguous, never single. The numeric
//! [`resolve_compat_issue`] projection treats each number as one
//! scope-unknown identity, which reproduces the historical behavior
//! for single-scope callers; scoped callers use
//! [`resolve_scoped_compat_issue`] so cross-scope duplicates stay
//! ambiguous there too.

//! P3 integration API.
#![allow(dead_code)]

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatIssue {
    pub issue_number: Option<u64>,
    pub ambiguous: bool,
    pub suppressed_default: bool,
}

/// One linked/detached row as seen by [`resolve_scoped_compat_issue`]:
/// the provider/project scope plus the numeric issue id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopedIssueRef<'a> {
    pub provider: &'a str,
    pub project: &'a str,
    pub issue_number: u64,
}

/// Resolve the compatible issue number from linked/detached issue
/// numbers plus the legacy Git binding. `linked` and `detached` are
/// the distinct `issue_number` values of the branch's link rows (already
/// scope-filtered by the caller, or all scopes when unscoped);
/// `legacy` is the Git-config binding when present.
pub fn resolve_compat_issue(
    linked: &[u64],
    detached: &[u64],
    legacy: Option<u64>,
    branch: &str,
    default_branch: Option<&str>,
) -> CompatIssue {
    // Numeric projection: every number is one scope-unknown identity,
    // so single-scope callers keep their historical answers exactly.
    let linked_ids: Vec<(String, String, u64)> = {
        let mut numbers = linked.to_vec();
        numbers.sort_unstable();
        numbers.dedup();
        numbers
            .into_iter()
            .map(|number| (String::new(), String::new(), number))
            .collect()
    };
    let mut detached_numbers = detached.to_vec();
    detached_numbers.sort_unstable();
    detached_numbers.dedup();
    resolve_core(
        &linked_ids,
        &detached_numbers,
        legacy,
        branch,
        default_branch,
    )
}

/// Resolve the compatible issue number from scope-qualified link rows
/// plus the legacy Git binding. Two rows that share a number under
/// different `(provider, project)` scopes are distinct identities and
/// resolve ambiguous instead of collapsing to that number; all other
/// semantics (legacy merge, detach suppression, default-branch
/// suppression) match [`resolve_compat_issue`].
pub fn resolve_scoped_compat_issue(
    linked: &[ScopedIssueRef<'_>],
    detached: &[ScopedIssueRef<'_>],
    legacy: Option<u64>,
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
    let mut detached_numbers: Vec<u64> = detached.iter().map(|entry| entry.issue_number).collect();
    detached_numbers.sort_unstable();
    detached_numbers.dedup();
    resolve_core(
        &linked_ids,
        &detached_numbers,
        legacy,
        branch,
        default_branch,
    )
}

/// Shared resolution over distinct `(provider, project, issue_number)`
/// identities plus the distinct detached numbers (which only gate the
/// scope-unknown legacy binding). More than one linked identity is
/// never guessed; otherwise the distinct numbers decide exactly like
/// the historical numeric rule.
fn resolve_core(
    linked_ids: &[(String, String, u64)],
    detached_numbers: &[u64],
    legacy: Option<u64>,
    branch: &str,
    default_branch: Option<&str>,
) -> CompatIssue {
    let mut numbers: Vec<u64> = linked_ids.iter().map(|(_, _, number)| *number).collect();
    if let Some(number) = legacy.filter(|number| !detached_numbers.contains(number)) {
        numbers.push(number);
    }
    numbers.sort_unstable();
    numbers.dedup();
    if default_branch.is_some_and(|default| default == branch) {
        return CompatIssue {
            issue_number: None,
            ambiguous: numbers.len() > 1 || linked_ids.len() > 1,
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
    match numbers.len() {
        1 => CompatIssue {
            issue_number: Some(numbers[0]),
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
