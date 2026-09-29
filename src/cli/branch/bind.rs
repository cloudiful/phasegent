//! Database-backed `issue bind` / `issue unbind` flows (issue 628 P3).
//!
//! Writes go to durable branch links only. Legacy Git keys are read
//! (idempotent import, fallback display) but never written or deleted
//! here. When no link scope can be selected without guessing, the flow
//! degrades to the legacy Git behavior instead of assigning the issue
//! to an unrelated provider/project.

use crate::branch_context::{GitRunner, ProcessGitRunner};
use crate::branch_links::{
    IssueKey, LinkParams, LinkScope, UnknownState, checkout_root, detach, detect_default_branch,
    ensure_schema, import_legacy_bindings, issues_for_branch, link, list_legacy_bindings,
    read_origin_url, resolve_link_scope, resolve_repo_key,
};
use crate::command::IssueCommand;
use crate::policy::Role;
use crate::providers::ProviderKind;

fn now_secs() -> i64 {
    crate::worktree::now_unix_secs().max(1)
}

fn storage_error(message: String) -> serde_json::Value {
    serde_json::json!({"kind": "storage", "message": message})
}

fn git_error(message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({"kind": "git", "message": message.into()})
}

/// Current branch or a structured legacy-compatible error value.
fn current_branch(runner: &dyn GitRunner) -> Result<String, serde_json::Value> {
    crate::branch_context::current_branch(runner).map_err(|error| error.json())
}

/// Durable repo key for this checkout: canonical origin, or the
/// local-only filesystem fallback (checkout root from git itself, so
/// the key is stable from any subdirectory) when no origin exists.
fn repo_key(
    runner: &dyn GitRunner,
) -> Result<crate::branch_links::ResolvedRepo, serde_json::Value> {
    let origin = read_origin_url(runner);
    resolve_repo_key(origin.as_deref(), &checkout_root(runner))
        .map_err(|error| super::scope_error(format!("cannot scope branch link: {error}")))
}

/// Idempotent legacy import under a redmine scope. Legacy keys are
/// `redmine-issue-id` values, so any other provider skips the import
/// (with a note) instead of misassigning redmine IDs elsewhere.
fn import_for_scope(
    connection: &rusqlite::Connection,
    runner: &dyn GitRunner,
    repo_key: &str,
    scope: &LinkScope,
    now: i64,
) -> Result<(usize, Option<String>), serde_json::Value> {
    if scope.provider != "redmine" {
        return Ok((
            0,
            Some("legacy redmine bindings need redmine scope; import skipped".to_owned()),
        ));
    }
    let bindings = list_legacy_bindings(runner).map_err(git_error)?;
    let summary = import_legacy_bindings(
        connection,
        repo_key,
        &bindings,
        &scope.provider,
        &scope.project,
        now,
    )
    .map_err(storage_error)?;
    Ok((summary.imported, None))
}

/// Database-backed `issue bind`. Falls back to the legacy Git behavior
/// when no scope can be selected without guessing.
pub(crate) fn execute_bind(
    role: Option<Role>,
    provider: Option<ProviderKind>,
    repository: Option<&str>,
    project_id: Option<&str>,
    issue_id: u64,
    replace: bool,
    session: Option<Box<str>>,
) -> i32 {
    let runner = ProcessGitRunner::new();
    let scope = match resolve_link_scope(role, provider, repository, project_id) {
        Ok(Some(scope)) => scope,
        Ok(None) => {
            return super::execute_branch_context(
                role,
                IssueCommand::Bind {
                    issue_id,
                    replace,
                    session,
                },
            );
        }
        Err(message) => return crate::cli::structured_error(super::scope_error(message), 1),
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
    let (imported, import_note) =
        match import_for_scope(&storage.connection, &runner, &resolved.key, &scope, now) {
            Ok(outcome) => outcome,
            Err(error) => return crate::cli::structured_error(error, 1),
        };
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
        Err(error) => return crate::cli::structured_error(super::scope_error(error), 1),
    };
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
    // With durable links a bind adds another link rather than replacing
    // history; `--replace` stays accepted for compatibility. No
    // auto-acquire hook runs here: it would write a legacy Git binding
    // as a side effect (dual-write) and lease flows belong to a later
    // phase. The legacy fallback below keeps the hook unchanged.
    let mut document = serde_json::json!({
        "bound": true,
        "branch": branch,
        "issue_id": issue_id,
        "replaced": replaced,
        "already_bound": already_bound,
        "provider": scope.provider,
        "project": scope.project,
        "repo_key": resolved.key,
        "local_only": resolved.local_only,
        "imported": imported,
    });
    if let Some(note) = import_note {
        document["import_note"] = serde_json::json!(note);
    }
    crate::cli::print_json(&document)
}

/// Database-backed `issue unbind`: detach every linked row for the
/// current branch in scope, retaining each as history. Legacy Git keys
/// are imported first (so the unlink has a record) but never deleted.
pub(crate) fn execute_unbind(
    role: Option<Role>,
    provider: Option<ProviderKind>,
    repository: Option<&str>,
    project_id: Option<&str>,
) -> i32 {
    let runner = ProcessGitRunner::new();
    let scope = match resolve_link_scope(role, provider, repository, project_id) {
        Ok(Some(scope)) => scope,
        Ok(None) => {
            return super::execute_branch_context(role, IssueCommand::Unbind);
        }
        Err(message) => return crate::cli::structured_error(super::scope_error(message), 1),
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
    let (imported, import_note) =
        match import_for_scope(&storage.connection, &runner, &resolved.key, &scope, now) {
            Ok(outcome) => outcome,
            Err(error) => return crate::cli::structured_error(error, 1),
        };
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
    // Read the branch view for the response without holding the storage
    // borrow across the print.
    let mut document = serde_json::json!({
        "unbound": detached > 0,
        "branch": branch,
        "detached": detached,
        "provider": scope.provider,
        "project": scope.project,
        "repo_key": resolved.key,
        "local_only": resolved.local_only,
        "imported": imported,
    });
    if detached == 0 {
        document["reason"] =
            serde_json::json!("no linked issues for this branch in scope; nothing detached");
    }
    if let Some(note) = import_note {
        document["import_note"] = serde_json::json!(note);
    }
    crate::cli::print_json(&document)
}
