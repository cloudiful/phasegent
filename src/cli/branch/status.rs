//! Database-backed `issue status` view (issue 628 P3).
//!
//! Pure read: durable links, the legacy Git binding, and the branch-name
//! fallback are projected together with each linked issue's last-known
//! local-index state (explicit unknown when missing) plus the reverse
//! branches of the active issue. The compatible `issue_id` is populated
//! only when unambiguous and the branch is not the detected default, so
//! adapter callers never receive a guessed active issue.

use crate::branch_context::ProcessGitRunner;
use crate::branch_links::compat::{ScopedIssueRef, resolve_scoped_compat_issue};
use crate::branch_links::{
    IssueKey, IssueStateLookup, LinkScope, LinkedBranch, LinkedIssue, StateSnapshot,
    branches_for_issue, checkout_root, detect_default_branch, ensure_schema, is_default_branch,
    issues_for_branch, read_origin_url, read_snapshot, resolve_link_scope, resolve_repo_key,
};
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

fn legacy_source(legacy: Option<u64>, branch: &str) -> &'static str {
    if legacy.is_some() {
        "bound"
    } else if crate::branch_context::parse_issue_id_from_branch_name(branch).is_some() {
        "named"
    } else {
        "none"
    }
}

/// Legacy-only view used when the link store is unreachable. Keeps the
/// compatible core (`branch`/`issue_id`/`source`) so adapter callers keep
/// working; the durable lists stay empty with a note.
fn legacy_only_view(branch: &str, legacy: Option<u64>, note: String) -> serde_json::Value {
    let named = crate::branch_context::parse_issue_id_from_branch_name(branch);
    serde_json::json!({
        "branch": branch,
        "issue_id": legacy.or(named),
        "source": legacy_source(legacy, branch),
        "scope": null,
        "legacy_binding": legacy.map(|issue_id| serde_json::json!({"issue_id": issue_id})),
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
    pub legacy: Option<u64>,
    pub repo_key: Option<&'a crate::branch_links::ResolvedRepo>,
    pub scope: Option<&'a LinkScope>,
    pub rows: &'a [LinkedIssue],
    pub reverse: &'a [LinkedBranch],
    pub default_branch: Option<&'a str>,
    pub compat: &'a crate::branch_links::CompatIssue,
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
    let mut document = serde_json::json!({
        "branch": view.branch,
        "issue_id": view.compat.issue_number,
        "source": legacy_source(view.legacy, view.branch),
        "default_branch": view.default_branch,
        "is_default_branch": is_default_branch(view.branch, view.default_branch),
        "ambiguous": view.compat.ambiguous,
        "suppressed_default": view.compat.suppressed_default,
        "legacy_binding": view.legacy.map(|issue_id| serde_json::json!({"issue_id": issue_id})),
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
/// legacy-only view (with a note) instead of failing the read.
pub(crate) fn execute_status(
    role: Option<Role>,
    provider: Option<ProviderKind>,
    repository: Option<&str>,
    project_id: Option<&str>,
) -> i32 {
    let runner = ProcessGitRunner::new();
    let branch = match crate::branch_context::current_branch(&runner) {
        Ok(branch) => branch,
        Err(error) => return crate::cli::structured_error(error.json(), 1),
    };
    // A malformed stored value keeps the legacy structured error.
    let legacy = match crate::branch_context::read_issue_id(&runner, &branch) {
        Ok(legacy) => legacy,
        Err(error) => return crate::cli::structured_error(error.json(), 1),
    };
    // The repo key shapes the durable read; its failure is not fatal
    // here. An unparseable origin or an unreachable store keeps the
    // legacy-compatible core. A scope selection failure (missing
    // project, stale stored provider) is a structured actionable
    // failure instead: only the legitimate no-provider case
    // (`Ok(None)`, nothing selected anywhere) keeps the unscoped
    // legacy read.
    let repo_key =
        resolve_repo_key(read_origin_url(&runner).as_deref(), &checkout_root(&runner)).ok();
    let scope: Option<LinkScope> = match resolve_link_scope(role, provider, repository, project_id)
    {
        Ok(scope) => scope,
        Err(message) => return crate::cli::structured_error(super::scope_error(message), 1),
    };
    let (resolved, storage) = match (&repo_key, crate::infra::storage::Storage::open()) {
        (Some(resolved), Ok(storage)) => match ensure_schema(&storage.connection) {
            Ok(()) => (resolved.clone(), storage),
            Err(error) => {
                return crate::cli::print_json(&legacy_only_view(
                    &branch,
                    legacy,
                    format!("branch link store unavailable: {error}"),
                ));
            }
        },
        _ => {
            return crate::cli::print_json(&legacy_only_view(
                &branch,
                legacy,
                "branch link store unavailable: local repository or database identity unresolved"
                    .to_owned(),
            ));
        }
    };
    let lookup = snapshot_lookup();
    let rows = match issues_for_branch(&storage.connection, &resolved.key, &branch, true, &lookup) {
        Ok(rows) => rows,
        Err(error) => {
            return crate::cli::print_json(&legacy_only_view(
                &branch,
                legacy,
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
    let mut detached: Vec<ScopedIssueRef> = Vec::new();
    for entry in rows.iter().filter(|entry| in_scope(entry)) {
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
    // Default-branch detection is cached-only (never a network query);
    // unknown detection keeps legacy-compatible reporting.
    let default_branch = detect_default_branch(&runner);
    let compat = resolve_scoped_compat_issue(
        &linked,
        &detached,
        legacy,
        &branch,
        default_branch.as_deref(),
    );
    // Reverse branches come only from durable rows (never constructed
    // from a legacy-only number, whose scope cannot be asserted here).
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
        legacy,
        repo_key: Some(&resolved),
        scope: scope.as_ref(),
        rows: &rows,
        reverse: &reverse,
        default_branch: default_branch.as_deref(),
        compat: &compat,
    }))
}
