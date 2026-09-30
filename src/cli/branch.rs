//! Local branch-issue executor: `issue bind` / `issue unbind` /
//! `issue status`.
//!
//! These commands never resolve a provider, but they are still role-aware:
//! an explicit non-orchestrator role must not run the two write commands,
//! while a missing role (Git hooks, manual repair, scripts) keeps the
//! historical passthrough. Every flow requires a resolvable link scope and
//! writes only durable branch links.

use crate::command::IssueCommand;
use crate::policy::Role;
use crate::providers::ProviderKind;

#[path = "branch/bind.rs"]
pub(crate) mod bind;
#[path = "branch/status.rs"]
pub(crate) mod status;

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

/// Scope-aware branch context dispatch. Invocation
/// provider/repository/project overrides select the durable link scope
/// without any provider construction or network access; when no scope can
/// be selected without guessing, every flow fails closed with the
/// structured scope error instead of assigning the issue to an unrelated
/// provider/project.
pub(crate) fn execute_branch_context_scoped(
    role: Option<Role>,
    provider: Option<ProviderKind>,
    repository: Option<&str>,
    project_id: Option<&str>,
    command: IssueCommand,
) -> i32 {
    if let Some(error) = permission_denial(role, &command) {
        return super::structured_error(error, 3);
    }
    match command {
        IssueCommand::Bind { issue_id } => {
            bind::execute_bind(role, provider, repository, project_id, issue_id)
        }
        IssueCommand::Unbind => bind::execute_unbind(role, provider, repository, project_id),
        IssueCommand::StatusBranch => {
            status::execute_status(role, provider, repository, project_id)
        }
        _ => unreachable!("branch context dispatch handles only local issue commands"),
    }
}
