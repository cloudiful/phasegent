//! Read-only reverse lookup `issue branches N` (issue 628 P5).
//!
//! Lists every branch linked to numeric issue `N` in this repository
//! across all provider/project scopes. Pure local read: durable links
//! plus each link's last-known local-index state (explicit `unknown`
//! when missing). No provider construction, no network, no writes.
//! Same-number rows from distinct scopes stay distinct and set the
//! top-level `ambiguous` flag instead of being collapsed.

use crate::branch_context::ProcessGitRunner;
use crate::branch_links::{
    IssueKey, IssueStateLookup, StateSnapshot, checkout_root, ensure_schema, read_origin_url,
    read_snapshot, resolve_repo_key,
};

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

/// Database-backed `issue branches N`. Storage or repository failures
/// are structured errors (there is no legacy Git fallback for a numeric
/// reverse lookup); an issue with no links yields an empty `branches`
/// list instead of an error.
pub(crate) fn execute_branches(number: u64) -> i32 {
    if number == 0 {
        return crate::cli::structured_error(
            serde_json::json!({
                "kind": "argument",
                "operation": "issue branches",
                "message": "issue branches requires a positive issue id",
            }),
            2,
        );
    }
    let runner = ProcessGitRunner::new();
    let resolved =
        match resolve_repo_key(read_origin_url(&runner).as_deref(), &checkout_root(&runner)) {
            Ok(resolved) => resolved,
            Err(error) => {
                return crate::cli::structured_error(
                    serde_json::json!({
                        "kind": "storage",
                        "operation": "issue branches",
                        "message": format!("local repository identity unresolved: {error}"),
                    }),
                    1,
                );
            }
        };
    let storage = match crate::infra::storage::Storage::open() {
        Ok(storage) => storage,
        Err(error) => {
            return crate::cli::structured_error(
                serde_json::json!({
                    "kind": "storage",
                    "operation": "issue branches",
                    "message": format!("branch link store unavailable: {error}"),
                }),
                1,
            );
        }
    };
    if let Err(error) = ensure_schema(&storage.connection) {
        return crate::cli::structured_error(
            serde_json::json!({
                "kind": "storage",
                "operation": "issue branches",
                "message": format!("branch link store unavailable: {error}"),
            }),
            1,
        );
    }
    let lookup = snapshot_lookup();
    let rows = match crate::branch_links::reads::branches_for_number(
        &storage.connection,
        &resolved.key,
        number,
        true,
        &lookup,
    ) {
        Ok(rows) => rows,
        Err(error) => {
            return crate::cli::structured_error(
                serde_json::json!({
                    "kind": "storage",
                    "operation": "issue branches",
                    "message": format!("branch link read failed: {error}"),
                }),
                1,
            );
        }
    };
    let mut branches = Vec::with_capacity(rows.len());
    let mut scopes = std::collections::BTreeSet::new();
    for entry in &rows {
        scopes.insert((
            entry.issue.provider.clone(),
            entry.issue.project.clone(),
            entry.issue.external_id.clone(),
        ));
        let (state, state_source, indexed_at) = project_state(&entry.state);
        branches.push(serde_json::json!({
            "branch": entry.branch,
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
        }));
    }
    crate::cli::print_json(&serde_json::json!({
        "issue": number,
        "repo_key": resolved.key,
        "local_only": resolved.local_only,
        "branches": branches,
        "ambiguous": scopes.len() > 1,
    }))
}
