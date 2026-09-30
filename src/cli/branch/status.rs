//! Database-backed `issue status` view.
//!
//! Pure read: active durable links and the branch-name fallback are
//! projected together with each linked issue's last-known local-index state
//! (explicit unknown when missing) plus the reverse branches of the active
//! issue. The compatible `issue_id` is populated only when unambiguous and
//! the branch is not the detected default, so adapter callers never receive
//! a guessed active issue.

use crate::branch_links::compat::{
    CompatIssue, ScopedIssueRef, named_branch_issue, resolve_scoped_compat_issue,
};
use crate::branch_links::{
    IssueKey, IssueStateLookup, LinkScope, LinkedBranch, LinkedIssue, StateSnapshot,
    branches_for_issue, checkout_root, detect_default_branch, ensure_schema, is_default_branch,
    issues_for_branch, read_origin_url, read_snapshot, resolve_link_scope, resolve_repo_key,
};
use crate::git_runner::ProcessGitRunner;
use crate::policy::Role;
use crate::providers::ProviderKind;

fn snapshot_lookup() -> impl IssueStateLookup {
    struct LocalIndex;
    impl IssueStateLookup for LocalIndex {
        fn lookup(&self, issue: &IssueKey) -> Option<StateSnapshot> {
            read_snapshot(&issue.provider, &issue.project, &issue.external_id).map(|found| {
                StateSnapshot {
                    state: found.state,
                    source: crate::branch_links::SNAPSHOT_SOURCE.to_owned(),
                    indexed_at: found.indexed_at,
                }
            })
        }
    }
    LocalIndex
}

fn project_state(state: &Option<StateSnapshot>) -> (String, String, Option<i64>) {
    match state {
        Some(snapshot) => (
            snapshot.state.clone(),
            snapshot.source.clone(),
            Some(snapshot.indexed_at),
        ),
        None => ("unknown".to_owned(), "unknown".to_owned(), None),
    }
}

fn project_issue(entry: &LinkedIssue) -> serde_json::Value {
    let (state, state_source, indexed_at) = project_state(&entry.state);
    serde_json::json!({
        "provider": entry.issue.provider,
        "project": entry.issue.project,
        "external_id": entry.issue.external_id,
        "issue_number": entry.issue_number,
        "status": entry.status,
        "source": entry.source,
        "detached_at": entry.detached_at,
        "detached_reason": entry.detached_reason,
        "state": state,
        "state_source": state_source,
        "indexed_at": indexed_at,
    })
}

fn project_branch(entry: &LinkedBranch) -> serde_json::Value {
    let (state, state_source, indexed_at) = project_state(&entry.state);
    serde_json::json!({
        "branch": entry.branch,
        "issue_number": entry.issue_number,
        "status": entry.status,
        "source": entry.source,
        "detached_at": entry.detached_at,
        "detached_reason": entry.detached_reason,
        "state": state,
        "state_source": state_source,
        "indexed_at": indexed_at,
    })
}

/// The reported active issue and how it resolved. A durable link is
/// `linked`; the branch name is `named`; no resolution is `none`.
/// Ambiguity and the detected default suppress both sources.
fn active_issue(
    branch: &str,
    default_branch: Option<&str>,
    compat: &CompatIssue,
) -> (Option<u64>, &'static str) {
    if compat.suppressed_default || compat.ambiguous {
        return (None, "none");
    }
    if let Some(number) = compat.issue_number {
        return (Some(number), "linked");
    }
    match named_branch_issue(branch, default_branch) {
        Some(number) => (Some(number), "named"),
        None => (None, "none"),
    }
}

/// Fallback view used when the link store is unreachable. Keeps the
/// compatible core (`branch`/`issue_id`/`source`) so adapter callers keep
/// working; the durable lists stay empty with a note.
fn store_unavailable_view(
    branch: &str,
    default_branch: Option<&str>,
    note: String,
) -> serde_json::Value {
    let named = named_branch_issue(branch, default_branch);
    serde_json::json!({
        "branch": branch,
        "issue_id": named,
        "source": if named.is_some() { "named" } else { "none" },
        "scope": null,
        "linked_issues": [],
        "linked_branches": [],
        "ambiguous": false,
        "storage_note": note,
    })
}

/// Inputs for [`status_document`], bundled to stay under the
/// argument-count lint.
pub(crate) struct StatusView<'a> {
    pub branch: &'a str,
    pub repo_key: Option<&'a crate::branch_links::ResolvedRepo>,
    pub scope: Option<&'a LinkScope>,
    pub rows: &'a [LinkedIssue],
    pub reverse: &'a [LinkedBranch],
    pub default_branch: Option<&'a str>,
    pub compat: &'a CompatIssue,
}

/// Pure status document assembly, separated from git/storage IO so
/// the shape is unit-testable without a checkout or database.
pub(crate) fn status_document(view: &StatusView<'_>) -> serde_json::Value {
    let in_scope = |entry: &LinkedIssue| match view.scope {
        Some(scope) => {
            entry.issue.provider == scope.provider && entry.issue.project == scope.project
        }
        None => true,
    };
    let (issue_id, source) = active_issue(view.branch, view.default_branch, view.compat);
    let mut document = serde_json::json!({
        "branch": view.branch,
        "issue_id": issue_id,
        "source": source,
        "default_branch": view.default_branch,
        "is_default_branch": is_default_branch(view.branch, view.default_branch),
        "ambiguous": view.compat.ambiguous,
        "suppressed_default": view.compat.suppressed_default,
        "linked_issues": view.rows.iter().filter(|entry| in_scope(entry)).map(project_issue).collect::<Vec<_>>(),
        "linked_branches": view.reverse.iter().map(project_branch).collect::<Vec<_>>(),
    });
    match view.repo_key {
        Some(resolved) => {
            document["repo_key"] = serde_json::json!(resolved.key);
            document["local_only"] = serde_json::json!(resolved.local_only);
        }
        None => {
            document["repo_key"] = serde_json::Value::Null;
            document["local_only"] = serde_json::Value::Null;
        }
    }
    document["scope"] = match view.scope {
        Some(scope) => serde_json::json!({
            "provider": scope.provider,
            "project": scope.project,
        }),
        None => serde_json::Value::Null,
    };
    document
}

/// Database-backed `issue status`. Storage failures degrade to the
/// branch-name-only view (with a note) instead of failing the read.
pub(crate) fn execute_status(
    role: Option<Role>,
    provider: Option<ProviderKind>,
    repository: Option<&str>,
    project_id: Option<&str>,
) -> i32 {
    let runner = ProcessGitRunner::new();
    let branch = match crate::git_runner::current_branch(&runner) {
        Ok(branch) => branch,
        Err(error) => return crate::cli::structured_error(error.json(), 1),
    };
    // Default-branch detection is cached-only (never a network query);
    // unknown detection keeps the branch-name fallback reporting.
    let default_branch = detect_default_branch(&runner);
    // The repo key and scope shape the durable read; neither failure is
    // fatal here. An unparseable origin, an unresolvable scope, or an
    // unreachable store keeps the branch-name-compatible core.
    let repo_key =
        resolve_repo_key(read_origin_url(&runner).as_deref(), &checkout_root(&runner)).ok();
    let scope: Option<LinkScope> = resolve_link_scope(role, provider, repository, project_id)
        .ok()
        .flatten();
    let (resolved, storage) = match (&repo_key, crate::infra::storage::Storage::open()) {
        (Some(resolved), Ok(storage)) => match ensure_schema(&storage.connection) {
            Ok(()) => (resolved.clone(), storage),
            Err(error) => {
                return crate::cli::print_json(&store_unavailable_view(
                    &branch,
                    default_branch.as_deref(),
                    format!("branch link store unavailable: {error}"),
                ));
            }
        },
        _ => {
            return crate::cli::print_json(&store_unavailable_view(
                &branch,
                default_branch.as_deref(),
                "branch link store unavailable: local repository or database identity unresolved"
                    .to_owned(),
            ));
        }
    };
    let lookup = snapshot_lookup();
    let rows = match issues_for_branch(&storage.connection, &resolved.key, &branch, true, &lookup) {
        Ok(rows) => rows,
        Err(error) => {
            return crate::cli::print_json(&store_unavailable_view(
                &branch,
                default_branch.as_deref(),
                format!("branch link read failed: {error}"),
            ));
        }
    };
    let in_scope = |entry: &LinkedIssue| match &scope {
        Some(scope) => {
            entry.issue.provider == scope.provider && entry.issue.project == scope.project
        }
        None => true,
    };
    let mut linked: Vec<ScopedIssueRef> = Vec::new();
    for entry in rows.iter().filter(|entry| in_scope(entry)) {
        if entry.status == crate::branch_links::store::STATUS_LINKED {
            linked.push(ScopedIssueRef {
                provider: &entry.issue.provider,
                project: &entry.issue.project,
                issue_number: entry.issue_number,
            });
        }
    }
    let compat = resolve_scoped_compat_issue(&linked, &branch, default_branch.as_deref());
    // Reverse branches come only from durable rows (never constructed
    // from a branch-name number, whose scope cannot be asserted here).
    let mut reverse: Vec<LinkedBranch> = Vec::new();
    if let Some(active) = compat.issue_number
        && let Some(key) = rows.iter().find_map(|entry| {
            (entry.issue_number == active
                && entry.status == crate::branch_links::store::STATUS_LINKED)
                .then(|| entry.issue.clone())
        })
    {
        reverse = branches_for_issue(&storage.connection, &resolved.key, &key, true, &lookup)
            .unwrap_or_default();
    }
    crate::cli::print_json(&status_document(&StatusView {
        branch: &branch,
        repo_key: Some(&resolved),
        scope: scope.as_ref(),
        rows: &rows,
        reverse: &reverse,
        default_branch: default_branch.as_deref(),
        compat: &compat,
    }))
}
