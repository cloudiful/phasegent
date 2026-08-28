use crate::policy::{Capability, Role};
use crate::providers::ProviderKind;
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;

/// Route `ci` reads to either Forgejo or GitLab. Phase 3 lifts the
/// Forgejo-only restriction so GitLab CI reads can reach the GitLab
/// provider; Redmine still rejects because it has no first-class CI
/// endpoint.
pub(crate) fn execute_ci_or_gitlab(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: crate::command::CiCommand,
) -> i32 {
    let role = super::required_role(role_value);
    let capability = Capability::CiRead;
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            crate::ci_cli::execute(role_value, api_base, repository, command)
        }
        Ok(ProviderKind::Gitlab) => match super::provider_for(
            role,
            Some(ProviderKind::Gitlab),
            api_base,
            repository,
            project_id,
            close_status_id,
        ) {
            Ok(provider) => super::print_result(provider.ci_for_command(&command)),
            Err(error) => super::provider_error(error),
        },
        Ok(ProviderKind::Redmine) => {
            super::provider_error(ForgejoError::not_supported("redmine", "ci read"))
        }
        Err(error) => super::provider_error(error),
    }
}
