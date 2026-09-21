//! Local branch-context executor: `issue bind` / `issue unbind` /
//! `issue status`.
//!
//! These commands never resolve a provider, but they are still role-aware:
//! an explicit non-orchestrator role must not run the two write commands,
//! while a missing role (Git hooks, manual repair, legacy scripts) keeps the
//! historical passthrough.

use crate::command::IssueCommand;
use crate::policy::Role;

/// The write operation a branch-context command performs, or `None` for the
/// read-only `issue status`.
fn restricted_operation(command: &IssueCommand) -> Option<&'static str> {
    match command {
        IssueCommand::Bind { .. } => Some("issue bind"),
        IssueCommand::Unbind => Some("issue unbind"),
        _ => None,
    }
}

/// Structured `permission` denial for an explicit non-orchestrator role on
/// `bind`/`unbind`. `None` means "let it through": `orchestrator` is allowed,
/// `status-branch` is unrestricted, and a role-less call keeps the historical
/// passthrough.
pub(crate) fn permission_denial(
    role: Option<Role>,
    command: &IssueCommand,
) -> Option<serde_json::Value> {
    let role = role?;
    if role == Role::Orchestrator {
        return None;
    }
    let operation = restricted_operation(command)?;
    Some(serde_json::json!({
        "kind": "permission",
        "role": role.as_str(),
        "operation": operation,
        "message": format!("role '{role}' is not allowed to perform {operation}"),
    }))
}

/// Whether a successful `issue bind` still needs the post-bind auto-acquire
/// hook. `already_bound: true` is the idempotent no-op path: the session
/// already owns that checkout, so acquiring again would only re-lease it. The
/// flag is read back from the unchanged bind document, which keeps
/// `execute_bind`'s signature and JSON contract untouched.
pub(crate) fn should_auto_acquire(bind_document: &serde_json::Value) -> bool {
    !bind_document
        .get("already_bound")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

pub(crate) fn execute_branch_context(role: Option<Role>, command: IssueCommand) -> i32 {
    if let Some(error) = permission_denial(role, &command) {
        return super::structured_error(error, 3);
    }
    let runner = crate::branch_context::ProcessGitRunner::new();
    let result = match command {
        IssueCommand::Bind {
            issue_id,
            replace,
            session,
        } => {
            // Issue 18: a successful bind is the session's task identity
            // becoming known, so run the shared best-effort auto-acquire hook.
            // It reports a created worktree (or a reuse warning) on stderr via
            // `report_local_warnings`; the stdout bind document is unchanged,
            // and no branch or worktree is ever deleted.
            let outcome = crate::branch_context::execute_bind(&runner, issue_id, replace);
            // A repeated bind for the same issue is an idempotent no-op, so it
            // must not re-lease the checkout the session already owns.
            if outcome.as_ref().is_ok_and(should_auto_acquire) {
                crate::cli::report_local_warnings(
                    "issue bind",
                    crate::worktree::auto_acquire_after_bind(issue_id, session.as_deref()),
                );
            }
            outcome
        }
        IssueCommand::Unbind => crate::branch_context::execute_unbind(&runner),
        IssueCommand::StatusBranch => crate::branch_context::execute_status(&runner),
        _ => unreachable!("branch context dispatch handles only local issue commands"),
    };
    match result {
        Ok(value) => super::print_json(&value),
        Err(error) => super::structured_error(error.json(), 1),
    }
}
