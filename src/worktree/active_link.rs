//! Durable branch-link issue context for lease decisions (issue 628 P4).
//!
//! Reads the linked issue identities for one `(repo_key, branch)` from
//! the branch-association store without guessing: no linked rows is
//! `None`, exactly one distinct `(provider, project, issue_number)`
//! identity is `Single`, and several is `Ambiguous`. Two rows that
//! share a numeric issue id under different provider/project scopes
//! are distinct identities (issue 628: they must never collapse into
//! one active issue) and therefore stay `Ambiguous`; only rows that
//! agree on all three parts resolve. Any storage failure (including a
//! missing table on databases that never ran the branch flows) is
//! also `None` so callers fall back to the legacy Git binding.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveLink {
    None,
    Single(u64),
    Ambiguous,
}

/// Linked issue identities for `(repo_key, branch)`, or `None` when
/// the store cannot answer.
pub(crate) fn resolve_active_link_issue(
    connection: &rusqlite::Connection,
    repo_key: &str,
    branch: &str,
) -> ActiveLink {
    if repo_key.trim().is_empty() || branch.trim().is_empty() {
        return ActiveLink::None;
    }
    let identities = match linked_identities(connection, repo_key, branch) {
        Some(identities) => identities,
        None => return ActiveLink::None,
    };
    match identities.len() {
        0 => ActiveLink::None,
        1 => ActiveLink::Single(identities[0].2),
        _ => ActiveLink::Ambiguous,
    }
}

/// Distinct `(provider, project, issue_number)` identities behind the
/// branch's linked rows. Scopes that share a number stay distinct
/// rows here so they resolve `Ambiguous` above; only full agreement
/// collapses to one identity.
fn linked_identities(
    connection: &rusqlite::Connection,
    repo_key: &str,
    branch: &str,
) -> Option<Vec<(String, String, u64)>> {
    let mut statement = connection
        .prepare(
            "SELECT DISTINCT provider, project, issue_number FROM branch_issue_links \
             WHERE repo_key = ?1 AND branch = ?2 AND status = 'linked'",
        )
        .ok()?;
    let rows = statement
        .query_map(rusqlite::params![repo_key, branch], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .ok()?;
    let mut out = Vec::new();
    for row in rows {
        if let Ok((provider, project, raw)) = row
            && raw > 0
        {
            out.push((provider, project, raw as u64));
        }
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

/// Durable repo key for a checkout at `repo_path`: the canonical
/// origin URL when one resolves, else the local-only filesystem
/// fallback. Returns `None` when neither can be established (notably
/// on an unparseable origin) so the caller falls back to the legacy
/// binding instead of guessing.
pub(crate) fn repo_key_for_checkout(
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &std::path::Path,
) -> Option<String> {
    let output = runner
        .run(&["remote", "get-url", "origin"], repo_path)
        .ok()?;
    let origin = if output.status == 0 && !output.stdout.trim().is_empty() {
        Some(output.stdout.trim().to_owned())
    } else {
        None
    };
    crate::branch_links::resolve_repo_key(origin.as_deref(), repo_path)
        .ok()
        .map(|resolved| resolved.key)
}

/// Branch ownership for the dirty-tree rules: the durable link when
/// it resolves unambiguously, else the legacy Git binding. `Ambiguous`
/// means the branch is linked to several issues and no guess is made;
/// callers must isolate instead of reusing the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BranchOwnership {
    Unbound { warning: Option<String> },
    Bound(u64),
    Ambiguous,
}

/// Resolve the issue the current branch belongs to, following the
/// `hooks.rs` `bound_issue_id` pattern but never erroring: durable
/// single links decide first (the database is authoritative), a
/// detached HEAD or any config lookup failure is treated as *unbound*
/// and returned as a warning string so the caller falls through the
/// decision table instead of failing the acquire.
pub(crate) fn resolve_branch_ownership(
    storage: &crate::infra::storage::Storage,
    runner: &dyn crate::worktree::WorktreeRunner,
    repo_path: &std::path::Path,
) -> BranchOwnership {
    let branch = match crate::worktree::git::current_branch_for(runner, repo_path) {
        Ok(branch) => branch,
        Err(error) => {
            return BranchOwnership::Unbound {
                warning: Some(format!(
                    "could not resolve current branch ({}); treating checkout as unbound",
                    error.message
                )),
            };
        }
    };
    if let Some(repo_key) = repo_key_for_checkout(runner, repo_path) {
        match resolve_active_link_issue(&storage.connection, &repo_key, &branch) {
            ActiveLink::Single(number) => return BranchOwnership::Bound(number),
            ActiveLink::Ambiguous => return BranchOwnership::Ambiguous,
            ActiveLink::None => {}
        }
    }
    let git_runner = crate::branch_context::ProcessGitRunner::in_directory(repo_path.to_path_buf());
    match crate::branch_context::read_issue_id(&git_runner, &branch) {
        Ok(Some(number)) => BranchOwnership::Bound(number),
        Ok(None) => BranchOwnership::Unbound { warning: None },
        Err(error) => BranchOwnership::Unbound {
            warning: Some(format!(
                "could not read binding for branch '{branch}' ({}); treating checkout as unbound",
                error.message
            )),
        },
    }
}
