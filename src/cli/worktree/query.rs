//! Read-only executors for `worktree status` and `worktree list`.
//!
//! Both share the read-role gate applied by the parent dispatcher and
//! surface directory registry records through the same [`DirectoryJson`]
//! shape, so a caller can page the same record from either entry point.
//! Neither path creates, migrates, or writes a store or lock file.

use serde::Serialize;

use crate::cli::{print_json, structured_error};
use crate::pagination::{PageRequest, Paged, truncation_warning};
use crate::worktree::registry::{load_records, page_records};
use crate::worktree::{DirectoryLegacyIssue, DirectoryRecord, DirectoryTask};

use super::{resolve_list_identity, storage_error};

#[derive(Debug, Serialize)]
pub(crate) struct DirectoryTaskJson {
    pub task_id: i64,
    pub issue: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

impl From<DirectoryTask> for DirectoryTaskJson {
    fn from(task: DirectoryTask) -> Self {
        Self {
            task_id: task.task_id,
            issue: task.issue,
            provider: task.provider,
            instance: task.instance,
            project: task.project,
            external_id: task.external_id,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct DirectoryLegacyJson {
    pub issue: u64,
    pub provider: String,
    pub project: String,
    pub external_id: u64,
}

impl From<DirectoryLegacyIssue> for DirectoryLegacyJson {
    fn from(issue: DirectoryLegacyIssue) -> Self {
        Self {
            issue: issue.issue,
            provider: issue.provider,
            project: issue.project,
            external_id: issue.external_id,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct DirectoryJson {
    pub directory_id: String,
    pub repo_identity: String,
    pub checkout_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub created_at: i64,
    pub tasks: Vec<DirectoryTaskJson>,
    pub legacy_issues: Vec<DirectoryLegacyJson>,
    pub sessions: Vec<String>,
}

impl From<DirectoryRecord> for DirectoryJson {
    fn from(record: DirectoryRecord) -> Self {
        Self {
            directory_id: record.directory_id,
            repo_identity: record.repo_identity,
            checkout_path: record.checkout_path,
            worktree_path: record.worktree_path,
            branch: record.branch,
            created_at: record.created_at,
            tasks: record
                .tasks
                .into_iter()
                .map(DirectoryTaskJson::from)
                .collect(),
            legacy_issues: record
                .legacy_issues
                .into_iter()
                .map(DirectoryLegacyJson::from)
                .collect(),
            sessions: record.sessions,
        }
    }
}

#[allow(dead_code)]
pub(super) fn execute_status(issue: u64) -> i32 {
    execute_status_page(issue, None)
}

pub(super) fn execute_status_page(issue: u64, page: Option<PageRequest>) -> i32 {
    crate::output::progress::stage("worktree status: reading the directory registry");
    let storage = match read_storage() {
        Ok(storage) => storage,
        Err(error) => return storage_error(&crate::worktree::WorktreeError::new("storage", error)),
    };
    match (storage.as_ref(), page) {
        (Some(storage), Some(page)) => match page_payload(storage, None, Some(issue), page) {
            Ok(payload) => print_json(&payload),
            Err(error) => storage_error(&error),
        },
        (Some(storage), None) => match load_records(storage, None) {
            Ok(rows) => {
                let mut rows: Vec<DirectoryRecord> = rows
                    .into_iter()
                    .filter(|row| row.has_issue(issue))
                    .collect();
                let total = rows.len();
                crate::cli::report_local_warnings(
                    "worktree status",
                    truncation_warning(total, rows.len()),
                );
                rows.truncate(100);
                print_json(&serde_json::json!({
                    "issue": issue,
                    "directories": rows.into_iter().map(DirectoryJson::from).collect::<Vec<_>>(),
                }))
            }
            Err(error) => storage_error(&error),
        },
        (None, Some(page)) => match empty_page(serde_json::json!({"issue": issue}), page) {
            Ok(payload) => print_json(&payload),
            Err(error) => storage_error(&error),
        },
        (None, None) => print_json(&serde_json::json!({
            "issue": issue,
            "directories": Vec::<DirectoryJson>::new(),
        })),
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
    crate::output::progress::stage("worktree list: reading the directory registry");
    if let Some(setup) = setup
        && let Some(scope) = setup.scopes.first()
    {
        return render_list(Some(&setup.storage), &scope.repo_identity, page);
    }
    let identity = match resolve_list_identity(repo, runner) {
        Ok(identity) => identity,
        Err(message) => return structured_error(message, 2),
    };
    let storage = match read_storage() {
        Ok(storage) => storage,
        Err(error) => {
            return storage_error(&crate::worktree::WorktreeError::new("storage", error));
        }
    };
    render_list(storage.as_ref(), &identity, page)
}

fn read_storage() -> Result<Option<crate::infra::storage::Storage>, String> {
    crate::infra::storage::Storage::open_existing_read_only()
}

fn render_list(
    storage: Option<&crate::infra::storage::Storage>,
    identity: &str,
    page: Option<PageRequest>,
) -> i32 {
    let payload = match (storage, page) {
        (Some(storage), Some(page)) => page_payload(storage, Some(identity), None, page),
        (Some(storage), None) => list_payload(storage, identity),
        (None, Some(page)) => empty_page(serde_json::json!({"repo_identity": identity}), page),
        (None, None) => Ok(serde_json::json!({
            "repo_identity": identity,
            "directories": Vec::<DirectoryJson>::new(),
        })),
    };
    match payload {
        Ok(payload) => print_json(&payload),
        Err(error) => storage_error(&error),
    }
}

fn empty_page(
    context: serde_json::Value,
    request: PageRequest,
) -> Result<serde_json::Value, crate::worktree::WorktreeError> {
    Paged::known(Vec::<DirectoryJson>::new(), request, 0)
        .with_context(context)
        .map_err(|message| crate::worktree::WorktreeError::new("serialization", message))
}

fn page_payload(
    storage: &crate::infra::storage::Storage,
    identity: Option<&str>,
    issue: Option<u64>,
    request: PageRequest,
) -> Result<serde_json::Value, crate::worktree::WorktreeError> {
    let (rows, total) = page_records(storage, identity, issue, request)?;
    let context = match identity {
        Some(identity) => serde_json::json!({"repo_identity": identity}),
        None => serde_json::json!({"issue": issue}),
    };
    Paged::known(
        rows.into_iter().map(DirectoryJson::from).collect(),
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
    let rows = load_records(storage, Some(identity))?;
    let total = rows.len();
    crate::cli::report_local_warnings("worktree list", truncation_warning(total, rows.len()));
    let mut rows = rows;
    rows.truncate(100);
    Ok(serde_json::json!({
        "repo_identity": identity,
        "directories": rows.into_iter().map(DirectoryJson::from).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_store_pages_as_a_truthful_empty_page() {
        let payload = empty_page(
            serde_json::json!({"repo_identity": "/repo/.git"}),
            PageRequest::new(1, 100).expect("page"),
        )
        .expect("empty page");
        assert_eq!(payload["items"], serde_json::json!([]));
        assert_eq!(payload["page"], serde_json::json!(1));
        assert_eq!(payload["limit"], serde_json::json!(100));
        assert_eq!(payload["total_count"], serde_json::json!(0));
        assert_eq!(payload["has_more"], serde_json::json!(false));
        assert_eq!(payload["repo_identity"], serde_json::json!("/repo/.git"));
    }
}
