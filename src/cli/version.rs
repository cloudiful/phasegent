use crate::command::VersionCommand;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::{IssueProvider, ProviderKind, RedmineMetadataProvider};

/// Redmine or local project version discovery. Every role may read
/// versions (planning is read-mostly), while Forgejo/GitLab reject the
/// operation with a structured not-supported error before any network
/// access. Local returns the empty catalogue without project discovery.
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
        // Phase 2 parity matrix (issue 257): GitLab now reports
        // `VersionRead = true` (via `GET /projects/:id/milestones`),
        // so the version list flows through the dispatcher and
        // renders the shared `RedmineVersion` shape (milestones map
        // onto Redmine versions). Forgejo stays not-supported.
        Ok(ProviderKind::Gitlab) => {}
        // Local returns the empty version catalogue via LocalProvider;
        // forgejo stays not-supported.
        Ok(ProviderKind::Local) => {}
        Err(error) => return super::provider_error(error),
    }
    // Repository-aware resolution for project-scoped reads.
    // Explicit --project-id wins; otherwise discover the project that
    // owns the current Git origin's mirror. Local skips discovery
    // entirely (no project id, no network) and lists the empty catalogue.
    let resolved_kind = match resolve_kind(role, provider_kind) {
        Ok(kind) => kind,
        Err(error) => return super::provider_error(error),
    };
    let resolved_project_id: Option<String> = if resolved_kind == ProviderKind::Local {
        None
    } else if project_id
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
                    "no Redmine project matches the current Git origin '{}'; pass --project-id or run 'phasegent --role admin --provider redmine admin workflow bootstrap'",
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
