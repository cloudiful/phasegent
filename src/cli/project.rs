use crate::command::ProjectCommand;
use crate::policy::{CallerContext, Capability, Role};
use crate::providers::api::PhasegentError;
use crate::providers::config::resolve_kind;
use crate::providers::{IssueProvider, ProviderKind, RedmineMetadataProvider};

#[allow(dead_code)]
pub(crate) fn execute_project(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: ProjectCommand,
) -> i32 {
    let caller = CallerContext::from_role(role_value);
    execute_project_with_caller(
        caller,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
        command,
    )
}

pub(crate) fn execute_project_with_caller(
    caller: CallerContext,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: ProjectCommand,
) -> i32 {
    if caller.is_managed_without_role() {
        let operation = match &command {
            ProjectCommand::List => Capability::ProjectRead.operation(),
            ProjectCommand::Create { .. } => Capability::ProjectCreate.operation(),
        };
        return super::managed_denial(operation);
    }
    let capability = match &command {
        ProjectCommand::List => Capability::ProjectRead,
        ProjectCommand::Create { .. } => Capability::ProjectCreate,
    };
    // All executable gates use the invocation context, not the resulting
    // credential role; the credential role only selects the stored key.
    if !caller.allows(capability) {
        let role = caller.credential_role().expect("agent has a role");
        return super::permission_error(role, capability);
    }
    let role = caller
        .credential_role()
        .expect("authorized caller has a credential");
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Redmine) | Ok(ProviderKind::Local) => {}
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
