//! Read-only executors for `worktree status` and `worktree list`.
//!
//! Both share the read-role gate applied by the parent dispatcher and
//! surface lease rows through the same [`LeaseJson`] shape, so a caller
//! can page the same record from either entry point.

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::worktree::leases::{ensure_schema, list_for_repo};
use crate::worktree::{LeaseRow, leases_for_issue};

use super::{config_error, open_storage, resolve_list_identity, storage_error};

/// One row of the JSON envelope returned by `status` and `list`.
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

pub(super) fn execute_status(issue: u64) -> i32 {
    match leases_for_issue(issue) {
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

pub(super) fn execute_list(repo: Option<&str>) -> i32 {
    let storage = match open_storage() {
        Ok(storage) => storage,
        Err(message) => return config_error(&message),
    };
    if let Err(message) = ensure_schema(&storage) {
        return config_error(&message);
    }
    let identity = match resolve_list_identity(&storage, repo) {
        Ok(identity) => identity,
        Err(message) => return structured_error(message, 2),
    };
    match list_for_repo(&storage, &identity) {
        Ok(rows) => {
            let payload = serde_json::json!({
                "repo_identity": identity,
                "leases": rows.into_iter().map(LeaseJson::from).collect::<Vec<_>>(),
            });
            print_json(&payload)
        }
        Err(error) => storage_error(&error),
    }
}
