use crate::command::StatusCommand;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::{
    IssueProvider, ProviderDispatcher, ProviderKind, RedmineMetadataProvider, RedmineProvider,
};

pub(crate) fn execute_status(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: StatusCommand,
) -> i32 {
    let role = super::required_role(role_value);
    let capability = Capability::IssueStatusRead;
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    // Status transitions drive the workflow lifecycle and are
    // orchestrator-owned, mirroring issue close; executors, reviewers,
    // and the admin bootstrap identity may not move an issue's status.
    // The check runs before any provider or network access so a denied
    // role fails fast with a structured permission error.
    if matches!(
        command,
        StatusCommand::Set { .. } | StatusCommand::Advance { .. }
    ) && role != Role::Orchestrator
    {
        return super::structured_error(
            serde_json::json!({
                "kind":"permission",
                "role":role.as_str(),
                "operation":"issue status update",
                "message":"issue status updates are orchestrator-only"
            }),
            3,
        );
    }
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            return super::provider_error(ForgejoError::not_supported(
                "forgejo",
                capability.operation(),
            ));
        }
        Ok(ProviderKind::Redmine) => {}
        // GitLab: list is unsupported (no native status enum), but
        // `set` maps to a managed workflow label update; the
        // orchestrator-only guard above already protects it.
        Ok(ProviderKind::Gitlab) => {}
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
    // GitLab exposes `IssueStatusRead` so the dispatch does not bail
    // out on the capability check; the per-command branch below
    // decides whether the call is supported. Redmine still uses the
    // capability surface for the actual work.
    if !provider.supports(capability) && !matches!(provider, ProviderDispatcher::Gitlab(_)) {
        return super::provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match command {
        StatusCommand::List => match provider {
            ProviderDispatcher::Gitlab(_) => {
                super::provider_error(ForgejoError::not_supported("gitlab", "issue status list"))
            }
            _ => super::print_result(provider.list_issue_statuses()),
        },
        // `next` and `advance` are Redmine-only capabilities: the
        // canonical policy is expressed with Redmine status names and
        // resolved against this installation's status ids.
        StatusCommand::Next { number } => match provider {
            ProviderDispatcher::Redmine(redmine) => {
                super::print_result(redmine.status_next(number))
            }
            other => super::provider_error(ForgejoError::not_supported(
                other.kind().as_str(),
                "issue status next",
            )),
        },
        StatusCommand::Advance { number, status } => match provider {
            ProviderDispatcher::Redmine(redmine) => {
                super::print_result(redmine.advance_issue_status(number, &status))
            }
            other => super::provider_error(ForgejoError::not_supported(
                other.kind().as_str(),
                "issue status advance",
            )),
        },
        StatusCommand::Set { number, status } => match provider {
            ProviderDispatcher::Gitlab(gitlab) => {
                super::print_result(gitlab.set_workflow_status(number, &status))
            }
            ProviderDispatcher::Redmine(redmine) => {
                let statuses = match redmine.list_issue_statuses() {
                    Ok(statuses) => statuses,
                    Err(error) => return super::provider_error(error),
                };
                let target = match RedmineProvider::select_status_by_value(&statuses, &status) {
                    Ok(target) => target,
                    Err(error) => return super::provider_error(error),
                };
                super::print_result(redmine.set_issue_status(number, target.id))
            }
            ProviderDispatcher::Forgejo(_) => super::provider_error(ForgejoError::not_supported(
                "forgejo",
                "issue status update",
            )),
        },
    }
}
