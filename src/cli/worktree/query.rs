//! Read-only executors for `worktree status` and `worktree list`.
//!
//! Both share the read-role gate applied by the parent dispatcher and
//! surface lease rows through the same [`LeaseJson`] shape, so a caller
//! can page the same record from either entry point.

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::pagination::{PageRequest, Paged, truncation_warning};
use crate::worktree::LeaseRow;
use crate::worktree::leases::{count_leases, list_for_issue, page_leases};
use crate::worktree::leases::{ensure_schema, list_for_repo};

use super::{config_error, open_storage, resolve_list_identity, storage_error};

#[derive(Debug, Serialize)]
pub(crate) struct LeaseJson {
    pub lease_id: String,
    pub repo_identity: String,
    pub issue: u64,
    pub session: String,
    pub checkout_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub status: String,
    pub created_at: i64,
    pub heartbeat_at: i64,
    /// Operator justification for a forced release; absent for
    /// ordinary releases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_reason: Option<String>,
}

impl From<LeaseRow> for LeaseJson {
    fn from(row: LeaseRow) -> Self {
        Self {
            lease_id: row.lease_id,
            repo_identity: row.repo_identity,
            issue: row.issue,
            session: row.session,
            checkout_path: row.checkout_path,
            worktree_path: row.worktree_path,
            branch: row.branch,
            status: row.status,
            created_at: row.created_at,
            heartbeat_at: row.heartbeat_at,
            release_reason: row.release_reason,
        }
    }
}

#[allow(dead_code)]
pub(super) fn execute_status(issue: u64) -> i32 {
    execute_status_page(issue, None)
}

pub(super) fn execute_status_page(issue: u64, page: Option<PageRequest>) -> i32 {
    crate::output::progress::stage("worktree status: reading the leases");
    let storage = match open_storage() {
        Ok(storage) => storage,
        Err(message) => {
            return storage_error(&crate::worktree::WorktreeError::new("storage", message));
        }
    };
    if let Err(message) = ensure_schema(&storage) {
        return storage_error(&crate::worktree::WorktreeError::new("storage", message));
    }
    if let Some(page) = page {
        return match page_payload(&storage, None, Some(issue), page) {
            Ok(payload) => print_json(&payload),
            Err(error) => storage_error(&error),
        };
    }
    match list_for_issue(&storage, issue).and_then(|rows| {
        let total = count_leases(&storage, None, Some(issue))?;
        crate::cli::report_local_warnings("worktree status", truncation_warning(total, rows.len()));
        Ok(rows)
    }) {
        Ok(rows) => {
            let payload = serde_json::json!({
                "issue": issue,
                "leases": rows.into_iter().map(LeaseJson::from).collect::<Vec<_>>(),
            });
            print_json(&payload)
        }
        Err(error) => storage_error(&error),
    }
}

#[allow(dead_code)]
pub(super) fn execute_list(repo: Option<&str>) -> i32 {
    execute_list_seeded(repo, None, &crate::worktree::ProcessWorktreeRunner::new())
}

pub(crate) fn execute_list_seeded(
    repo: Option<&str>,
    setup: Option<&crate::cli::issue::sync::SyncSetup>,
    runner: &dyn crate::worktree::WorktreeRunner,
) -> i32 {
    execute_list_page(repo, setup, runner, None)
}

pub(crate) fn execute_list_page(
    repo: Option<&str>,
    setup: Option<&crate::cli::issue::sync::SyncSetup>,
    runner: &dyn crate::worktree::WorktreeRunner,
    page: Option<PageRequest>,
) -> i32 {
    crate::output::progress::stage("worktree list: reading the leases");
    if let Some(setup) = setup
        && let Some(scope) = setup.scopes.first()
    {
        return render_list(&setup.storage, &scope.repo_identity, page);
    }
    let storage = match open_storage() {
        Ok(storage) => storage,
        Err(message) => return config_error(&message),
    };
    if let Err(message) = ensure_schema(&storage) {
        return config_error(&message);
    }
    let identity = match resolve_list_identity(&storage, repo, runner) {
        Ok(identity) => identity,
        Err(message) => return structured_error(message, 2),
    };
    render_list(&storage, &identity, page)
}

fn render_list(
    storage: &crate::infra::storage::Storage,
    identity: &str,
    page: Option<PageRequest>,
) -> i32 {
    let payload = match page {
        Some(page) => page_payload(storage, Some(identity), None, page),
        None => list_payload(storage, identity),
    };
    match payload {
        Ok(payload) => print_json(&payload),
        Err(error) => storage_error(&error),
    }
}

fn page_payload(
    storage: &crate::infra::storage::Storage,
    identity: Option<&str>,
    issue: Option<u64>,
    request: PageRequest,
) -> Result<serde_json::Value, crate::worktree::WorktreeError> {
    let (rows, total) = page_leases(storage, identity, issue, request)?;
    let context = match identity {
        Some(identity) => serde_json::json!({"repo_identity": identity}),
        None => serde_json::json!({"issue": issue}),
    };
    Paged::known(
        rows.into_iter().map(LeaseJson::from).collect(),
        request,
        total,
    )
    .with_context(context)
    .map_err(|message| crate::worktree::WorktreeError::new("serialization", message))
}

pub(crate) fn list_payload(
    storage: &crate::infra::storage::Storage,
    identity: &str,
) -> Result<serde_json::Value, crate::worktree::WorktreeError> {
    let rows = list_for_repo(storage, identity)?;
    let total = count_leases(storage, Some(identity), None)?;
    crate::cli::report_local_warnings("worktree list", truncation_warning(total, rows.len()));
    Ok(serde_json::json!({
        "repo_identity": identity,
        "leases": rows.into_iter().map(LeaseJson::from).collect::<Vec<_>>(),
    }))
}
