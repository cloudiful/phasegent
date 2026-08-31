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
    // Phase 3: repository-aware resolution for project-scoped reads.
    // Explicit --project-id wins; otherwise discover the project that
    // owns the current Git origin's mirror. A unique match supplies the
    // project id, NoMatch returns an actionable error, and Multiple or
    // any discovery HTTP/auth error is propagated. Reads never
    // auto-bootstrap.
    let resolved_project_id: Option<String> = if project_id
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        project_id.map(str::to_owned)
    } else {
        match super::project_resolution::resolve_redmine_project(
            role,
            api_base,
            repository,
            project_id,
            close_status_id,
        ) {
            Ok(Some(id)) => Some(id),
            Ok(None) => {
                let origin = crate::remote::resolve_origin()
                    .map(|remote| remote.repository)
                    .unwrap_or_else(|_| "current Git origin".to_owned());
                return super::provider_error(ForgejoError::config(format!(
                    "no Redmine project matches the current Git origin '{}'; pass --project-id or run 'phasegent --role admin --provider redmine workflow bootstrap'",
                    origin
                )));
            }
            Err(error) => return super::provider_error(error),
        }
    };
    let provider = match super::provider_for(
        role,
        provider_kind,
        api_base,
        repository,
        resolved_project_id.as_deref(),
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
