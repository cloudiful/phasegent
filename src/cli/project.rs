use crate::command::ProjectCommand;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::{IssueProvider, ProviderKind, RedmineMetadataProvider};

pub(crate) fn execute_project(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: ProjectCommand,
) -> i32 {
    let (role, capability) = match &command {
        ProjectCommand::List => (super::required_role(role_value), Capability::ProjectRead),
        ProjectCommand::Create { .. } => {
            (super::required_role(role_value), Capability::ProjectCreate)
        }
    };
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            return super::provider_error(ForgejoError::not_supported(
                "forgejo",
                capability.operation(),
            ));
        }
        Ok(ProviderKind::Redmine) => {}
        Ok(ProviderKind::Gitlab) => {
            return super::provider_error(ForgejoError::not_supported(
                "gitlab",
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
        return super::provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match command {
        ProjectCommand::List => super::print_result(provider.list_projects()),
        ProjectCommand::Create {
            name,
            identifier,
            description,
            confirmed,
        } => {
            if !confirmed {
                return super::structured_error(
                    serde_json::json!({
                        "kind":"authorization",
                        "operation":"project create",
                        "message":"project creation requires --confirm"
                    }),
                    2,
                );
            }
            super::print_result(provider.create_project(&name, &identifier, description.as_deref()))
        }
    }
}
