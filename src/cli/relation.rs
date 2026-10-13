use crate::command::RelationCommand;
use crate::policy::{CallerContext, Capability, Role};
use crate::providers::api::PhasegentError;
use crate::providers::config::resolve_kind;
use crate::providers::{IssueProvider, ProviderKind};

/// Redmine issue relations. `list` is available to every non-admin
/// role (orchestrator/executor/reviewer), while `create` and `delete` are
/// orchestrator-only; the admin identity is denied all three. Local
/// rejects every relation operation with a structured not-supported error
/// before any network access.
#[allow(dead_code)]
pub(crate) fn execute_relation(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: RelationCommand,
) -> i32 {
    let caller = CallerContext::from_role(role_value);
    execute_relation_with_caller(
        caller,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
        command,
    )
}

pub(crate) fn execute_relation_with_caller(
    caller: CallerContext,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: RelationCommand,
) -> i32 {
    let capability = match &command {
        RelationCommand::List { .. } => Capability::RelationRead,
        RelationCommand::Create { .. } => Capability::RelationCreate,
        RelationCommand::Delete { .. } => Capability::RelationDelete,
    };
    if caller.is_managed_without_role() {
        return super::managed_denial(capability.operation());
    }
    if !caller.allows(capability) {
        let role = caller.credential_role().expect("agent has a role");
        return super::permission_error(role, capability);
    }
    let role = caller
        .credential_role()
        .expect("authorized caller has a credential");
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Redmine) => {}
        // Local has no issue-relations surface; keep the structured
        // not-supported error before any provider build so the only side
        // effect is the structured error.
        Ok(ProviderKind::Local) => {
            return super::provider_error(PhasegentError::not_supported(
                "local",
                capability.operation(),
            ));
        }
        Err(error) => return super::provider_error(error),
    }
    let provider = match super::provider_for(
        role,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
    ) {
        Ok(provider) => provider,
        Err(error) => return super::provider_error(error),
    };
    if !provider.supports(capability) {
        return super::provider_error(PhasegentError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match crate::providers::redmine::relations::execute(&provider, &command) {
        Ok(crate::providers::redmine::relations::RelationResult::List(relations)) => {
            super::print_json(&relations)
        }
        Ok(crate::providers::redmine::relations::RelationResult::Created(summary)) => {
            super::print_json(&summary)
        }
        Ok(crate::providers::redmine::relations::RelationResult::Deleted(relation_id)) => {
            super::print_json(
                &serde_json::json!({"deleted": relation_id, "relation_id": relation_id}),
            )
        }
        Err(error) => super::provider_error(error),
    }
}
