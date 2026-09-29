use crate::policy::{Capability, Role};
use crate::providers::ProviderKind;
use crate::providers::api::ProviderError;
use crate::providers::config::resolve_kind;

/// Route `repo create` by resolved provider kind. GitLab reaches the
/// GitLab provider; Redmine and Local reject the operation because
/// they have no first-class repository endpoint. A missing provider
/// selection fails with the resolver's actionable config error; no
/// other provider is selected implicitly.
pub(crate) fn execute_repo_or_gitlab(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: crate::command::RepoCommand,
) -> i32 {
    let role = super::required_role(role_value);
    let capability = Capability::RepoCreate;
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Gitlab) => match super::provider_for(
            role,
            Some(ProviderKind::Gitlab),
            api_base,
            project_id,
            close_status_id,
        ) {
            Ok(provider) => {
                super::print_result(provider.create_repo_for_command(&command, role, api_base))
            }
            Err(error) => super::provider_error(error),
        },
        Ok(ProviderKind::Redmine) => {
            super::provider_error(ProviderError::not_supported("redmine", "repo create"))
        }
        // Local has no first-class repository endpoint, so repo
        // creation stays a structured not-supported error (mirrors the
        // Redmine arm).
        Ok(ProviderKind::Local) => {
            super::provider_error(ProviderError::not_supported("local", "repo create"))
        }
        Err(error) => super::provider_error(error),
    }
}
