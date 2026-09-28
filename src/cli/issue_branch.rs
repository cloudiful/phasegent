//! Best-effort branch-link retention for `issue close` (issue 628 P3).
//!
//! Closing retains every durable association: database links are never
//! detached, and this checkout's legacy keys are idempotently imported
//! under the close scope — before the unchanged legacy Git unbind runs —
//! so the closed link stays queryable afterward. All failures degrade
//! silently (or to a bounded stderr warning); stdout and the exit code
//! are never affected.

use crate::branch_context::ProcessGitRunner;
use crate::branch_links::{
    checkout_root, ensure_schema, import_legacy_bindings, list_legacy_bindings, read_origin_url,
    resolve_repo_key,
};
use crate::providers::ProviderDispatcher;
use crate::providers::index_store::provider_scope;

fn bound_warning(reason: &str) -> String {
    const MAX: usize = 200;
    let mut text = reason.trim().replace(['\n', '\r'], " ");
    while text.contains("  ") {
        text = text.replace("  ", " ");
    }
    if text.len() > MAX {
        let mut end = MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Import this checkout's legacy bindings under the close scope so the
/// closed association stays queryable. Never detaches anything; returns
/// a bounded warning when the import itself fails after the store
/// opened, else `None` (silent success or silent skip).
pub(crate) fn retain_closed_issue_links(
    provider: &ProviderDispatcher,
    _issue_number: u64,
) -> Option<String> {
    let scope = provider_scope(provider).ok()?;
    // Legacy keys are `redmine-issue-id` values; any other scope skips
    // the import instead of misassigning redmine IDs elsewhere.
    if scope.source != "redmine" {
        return None;
    }
    let runner = ProcessGitRunner::new();
    let repo_key =
        resolve_repo_key(read_origin_url(&runner).as_deref(), &checkout_root(&runner)).ok()?;
    let storage = crate::infra::storage::Storage::open().ok()?;
    if ensure_schema(&storage.connection).is_err() {
        return None;
    }
    let bindings = list_legacy_bindings(&runner).ok()?;
    let now = crate::worktree::now_unix_secs().max(1);
    match import_legacy_bindings(
        &storage.connection,
        &repo_key.key,
        &bindings,
        &scope.source,
        &scope.project,
        now,
    ) {
        Ok(_) => None,
        Err(error) => Some(bound_warning(&format!(
            "branch link retention skipped: {error}"
        ))),
    }
}
