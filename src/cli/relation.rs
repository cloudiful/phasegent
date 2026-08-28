use crate::command::RelationCommand;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::{IssueProvider, ProviderKind};

/// Redmine or GitLab issue relations. `list` is available to every non-admin
/// role (orchestrator/executor/reviewer), while `create` and `delete` are
/// orchestrator-only; the admin identity is denied all three. Forgejo
/// rejects every relation operation with a structured not-supported error
/// before any network access. Phase 4 lifts the GitLab foundation
/// restriction so the dispatch path also handles GitLab's
/// `/links` endpoint, with `precedes` and `--delay` rejected as
/// structured config errors rather than silently mapped.
pub(crate) fn execute_relation(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: RelationCommand,
) -> i32 {
    let role = super::required_role(role_value);
    let capability = match &command {
        RelationCommand::List { .. } => Capability::RelationRead,
        RelationCommand::Create { .. } => Capability::RelationCreate,
        RelationCommand::Delete { .. } => Capability::RelationDelete,
    };
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    // Forgejo has no issue relations; reject before any provider build or
    // network access so the structured not-supported error is the only side
    // effect. Redmine and GitLab both continue; the dispatch layer in
    // `redmine_relations_cli` validates provider-specific flags.
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            return super::provider_error(ForgejoError::not_supported(
                "forgejo",
                capability.operation(),
            ));
        }
        Ok(ProviderKind::Redmine) | Ok(ProviderKind::Gitlab) => {}
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
        return super::provider_error(ForgejoError::not_supported(
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
