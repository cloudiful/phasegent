use crate::command::VersionCommand;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::{IssueProvider, ProviderKind, RedmineMetadataProvider};

/// Redmine-only project version discovery. Every role may read versions
/// (planning is read-mostly), while Forgejo rejects the operation with a
/// structured not-supported error before any network access.
pub(crate) fn execute_version(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: VersionCommand,
) -> i32 {
    let role = super::required_role(role_value);
    let capability = Capability::VersionRead;
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
        VersionCommand::List => super::print_result(provider.list_project_versions()),
    }
}
