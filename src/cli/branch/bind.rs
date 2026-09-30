//! Database-backed `issue bind` / `issue unbind` flows.
//!
//! Writes go to durable branch links only. A bind requires a resolvable
//! provider/project scope: without one the flow fails closed with the
//! structured scope error instead of assigning the issue to an unrelated
//! provider/project, and it never touches local Git config.

use crate::branch_links::{
    IssueKey, LinkParams, LinkScope, UnknownState, checkout_root, detach, detect_default_branch,
    ensure_schema, issues_for_branch, link, read_origin_url, resolve_link_scope, resolve_repo_key,
};
use crate::git_runner::{GitRunner, ProcessGitRunner};
use crate::policy::Role;
use crate::providers::ProviderKind;

fn now_secs() -> i64 {
    crate::worktree::now_unix_secs().max(1)
}

fn scope_error(message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({"kind": "scope", "message": message.into()})
}

fn storage_error(message: String) -> serde_json::Value {
    serde_json::json!({"kind": "storage", "message": message})
}

/// Current branch or a structured error value.
fn current_branch(runner: &dyn GitRunner) -> Result<String, serde_json::Value> {
    crate::git_runner::current_branch(runner).map_err(|error| error.json())
}

/// Durable repo key for this checkout: canonical origin, or the
/// local-only filesystem fallback (checkout root from git itself, so
/// the key is stable from any subdirectory) when no origin exists.
fn repo_key(
    runner: &dyn GitRunner,
) -> Result<crate::branch_links::ResolvedRepo, serde_json::Value> {
    let origin = read_origin_url(runner);
    resolve_repo_key(origin.as_deref(), &checkout_root(runner))
        .map_err(|error| scope_error(format!("cannot scope branch link: {error}")))
}

/// Resolve the durable link scope, or a structured scope error. A missing
/// selection is a `scope` error, never a guess and never a Git-config
/// fallback.
fn link_scope(
    role: Option<Role>,
    provider: Option<ProviderKind>,
    repository: Option<&str>,
    project_id: Option<&str>,
) -> Result<LinkScope, serde_json::Value> {
    match resolve_link_scope(role, provider, repository, project_id) {
        Ok(Some(scope)) => Ok(scope),
        Ok(None) => Err(scope_error(
            "cannot scope branch link: no provider is selected; pass --provider with \
             --repository/--project-id or store a role provider; refusing to guess a project",
        )),
        Err(message) => Err(scope_error(message)),
    }
}

/// Database-backed `issue bind`. Requires a resolvable scope.
pub(crate) fn execute_bind(
    role: Option<Role>,
    provider: Option<ProviderKind>,
    repository: Option<&str>,
    project_id: Option<&str>,
    issue_id: u64,
) -> i32 {
    let runner = ProcessGitRunner::new();
    let scope = match link_scope(role, provider, repository, project_id) {
        Ok(scope) => scope,
        Err(error) => return crate::cli::structured_error(error, 1),
    };
    let branch = match current_branch(&runner) {
        Ok(branch) => branch,
        Err(error) => return crate::cli::structured_error(error, 1),
    };
    let resolved = match repo_key(&runner) {
        Ok(resolved) => resolved,
        Err(error) => return crate::cli::structured_error(error, 1),
    };
    let storage = match crate::infra::storage::Storage::open() {
        Ok(storage) => storage,
        Err(error) => return crate::cli::structured_error(storage_error(error), 1),
    };
    if let Err(error) = ensure_schema(&storage.connection) {
        return crate::cli::structured_error(storage_error(error), 1);
    }
    // The detected default branch can never be an active issue branch,
    // and neither can a conventional default name while detection is
    // unknown (cached-only detection; an undetected `main` is still
    // `main`). Never a network query to decide this.
    let default_branch = detect_default_branch(&runner);
    if let Some(default) = default_branch.as_deref()
        && default == branch
    {
        return crate::cli::structured_error(
            serde_json::json!({
                "kind": "branch",
                "branch": branch,
                "message": format!(
                    "branch '{branch}' is the detected default branch and cannot be bound to an active issue"
                ),
            }),
            1,
        );
    }
    if default_branch.is_none() && crate::branch_links::identity::is_protected_branch(&branch, None)
    {
        return crate::cli::structured_error(
            serde_json::json!({
                "kind": "branch",
                "branch": branch,
                "message": format!(
                    "branch '{branch}' looks like a conventional default branch and the cached \
                     remote HEAD is unknown; refusing to bind it to an active issue"
                ),
            }),
            1,
        );
    }
    let issue = match IssueKey::from_number(&scope.provider, &scope.project, issue_id) {
        Ok(issue) => issue,
        Err(error) => return crate::cli::structured_error(scope_error(error), 1),
    };
    let now = now_secs();
    let outcome = match link(
        &storage.connection,
        &LinkParams {
            repo_key: &resolved.key,
            branch: &branch,
            issue: &issue,
            issue_number: issue_id,
            source: "cli-bind",
            now,
        },
    ) {
        Ok(outcome) => outcome,
        Err(error) => return crate::cli::structured_error(storage_error(error), 1),
    };
    let (already_bound, replaced) = match outcome {
        crate::branch_links::LinkOutcome::AlreadyLinked => (true, false),
        crate::branch_links::LinkOutcome::Relinked => (false, true),
        crate::branch_links::LinkOutcome::Created => (false, false),
    };
    // A bind adds another link rather than replacing history; `replaced`
    // only reports a detached row that this bind re-activated.
    crate::cli::print_json(&serde_json::json!({
        "bound": true,
        "branch": branch,
        "issue_id": issue_id,
        "replaced": replaced,
        "already_bound": already_bound,
        "provider": scope.provider,
        "project": scope.project,
        "repo_key": resolved.key,
        "local_only": resolved.local_only,
    }))
}

/// Database-backed `issue unbind`: detach every linked row for the
/// current branch in scope, retaining each as history.
pub(crate) fn execute_unbind(
    role: Option<Role>,
    provider: Option<ProviderKind>,
    repository: Option<&str>,
    project_id: Option<&str>,
) -> i32 {
    let runner = ProcessGitRunner::new();
    let scope = match link_scope(role, provider, repository, project_id) {
        Ok(scope) => scope,
        Err(error) => return crate::cli::structured_error(error, 1),
    };
    let branch = match current_branch(&runner) {
        Ok(branch) => branch,
        Err(error) => return crate::cli::structured_error(error, 1),
    };
    let resolved = match repo_key(&runner) {
        Ok(resolved) => resolved,
        Err(error) => return crate::cli::structured_error(error, 1),
    };
    let storage = match crate::infra::storage::Storage::open() {
        Ok(storage) => storage,
        Err(error) => return crate::cli::structured_error(storage_error(error), 1),
    };
    if let Err(error) = ensure_schema(&storage.connection) {
        return crate::cli::structured_error(storage_error(error), 1);
    }
    let now = now_secs();
    let rows = match issues_for_branch(
        &storage.connection,
        &resolved.key,
        &branch,
        false,
        &UnknownState,
    ) {
        Ok(rows) => rows,
        Err(error) => return crate::cli::structured_error(storage_error(error), 1),
    };
    let mut detached = 0usize;
    for entry in rows.iter().filter(|entry| {
        entry.issue.provider == scope.provider && entry.issue.project == scope.project
    }) {
        match detach(
            &storage.connection,
            &resolved.key,
            &branch,
            &entry.issue,
            "unbind",
            now,
        ) {
            Ok(_) => detached += 1,
            Err(error) => return crate::cli::structured_error(storage_error(error), 1),
        }
    }
    let mut document = serde_json::json!({
        "unbound": detached > 0,
        "branch": branch,
        "detached": detached,
        "provider": scope.provider,
        "project": scope.project,
        "repo_key": resolved.key,
        "local_only": resolved.local_only,
    });
    if detached == 0 {
        document["reason"] =
            serde_json::json!("no linked issues for this branch in scope; nothing detached");
    }
    crate::cli::print_json(&document)
}
