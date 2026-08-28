use crate::command::WorkflowCommand;
use crate::policy::Role;
use crate::providers::ProviderKind;
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::workflow;

pub(crate) fn execute_workflow(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    global_repository: Option<&str>,
    global_close_status_id: Option<&str>,
    global_close_status_name: Option<&str>,
    command: WorkflowCommand,
) -> i32 {
    let role = super::required_role(role_value);
    if role != Role::Admin {
        return super::structured_error(
            serde_json::json!({
                "kind":"permission",
                "role":role.as_str(),
                "operation":"workflow bootstrap",
                "message":"workflow bootstrap is admin-only"
            }),
            3,
        );
    }
    let provider = match resolve_kind(role, provider_kind) {
        Ok(provider) => provider,
        Err(error) => return super::provider_error(error),
    };
    if provider != ProviderKind::Redmine {
        return super::provider_error(ForgejoError::not_supported(
            provider.as_str(),
            "workflow bootstrap",
        ));
    }
    let WorkflowCommand::Bootstrap {
        repository,
        close_status_id,
        close_status_name,
    } = command;
    if close_status_id.is_some() && global_close_status_id.is_some()
        || close_status_name.is_some() && global_close_status_name.is_some()
    {
        return super::usage_error("workflow bootstrap received a duplicate close-status option");
    }
    let close_status_id = close_status_id.or_else(|| global_close_status_id.map(str::to_owned));
    let close_status_name =
        close_status_name.or_else(|| global_close_status_name.map(str::to_owned));
    if close_status_id.is_some() && close_status_name.is_some() {
        return super::usage_error(
            "workflow bootstrap accepts either --close-status-id or --close-status-name",
        );
    }
    let result = match workflow::bootstrap(
        role,
        api_base,
        repository.as_deref().or(global_repository),
        close_status_id.as_deref(),
        close_status_name.as_deref(),
    ) {
        Ok(result) => result,
        Err(error) => return super::provider_error(error),
    };
    if !result.ready() {
        return print_bootstrap_warning(&result);
    }
    // A matching checkout whose hook installation failed still bootstraps
    // successfully; surface the bounded reason on stderr next to the JSON.
    if let Some(hooks) = &result.hooks {
        super::report_local_warnings("workflow bootstrap", hooks.warning());
    }
    super::print_json(&bootstrap_success_json(&result))
}

fn bootstrap_success_json(result: &workflow::BootstrapResult) -> serde_json::Value {
    serde_json::json!({
        "bootstrapped": true,
        "created": result.bootstrap.created,
        "repository": result.repository,
        "identifier": result.identifier,
        "project_id": result.bootstrap.project.id,
        "close_status_id": result.bootstrap.close_status.id,
        "close_status_name": result.bootstrap.close_status.name,
        "user_memberships": result.user_memberships.iter().map(|outcome| {
            serde_json::json!({
                "role": outcome.role_name,
                "user_id": outcome.user_id,
                "user_login": outcome.user_login,
                "status": outcome.status,
            })
        }).collect::<Vec<_>>(),
        "git_mirror": result.git_mirror.as_ref().map(git_mirror_json),
        "hooks": result.hooks.as_ref().map(crate::lifecycle::HookAutoInstall::to_json),
    })
}

fn print_bootstrap_warning(result: &workflow::BootstrapResult) -> i32 {
    let payload = serde_json::json!({
        "bootstrapped": false,
        "created": result.bootstrap.created,
        "repository": result.repository,
        "identifier": result.identifier,
        "project_id": result.bootstrap.project.id,
        "close_status_id": result.bootstrap.close_status.id,
        "close_status_name": result.bootstrap.close_status.name,
        "user_memberships": result.user_memberships.iter().map(|outcome| {
            serde_json::json!({
                "role": outcome.role_name,
                "user_id": outcome.user_id,
                "user_login": outcome.user_login,
                "status": outcome.status,
                "warning": outcome.warning,
            })
        }).collect::<Vec<_>>(),
        "git_mirror": result.git_mirror.as_ref().map(git_mirror_json),
        "hooks": result.hooks.as_ref().map(crate::lifecycle::HookAutoInstall::to_json),
        "warning": result
            .user_memberships
            .iter()
            .find_map(|outcome| outcome.warning.clone())
            .unwrap_or_else(|| "Redmine direct user memberships could not be ensured".to_owned()),
    });
    let printed = super::print_json(&payload);
    if printed == 0 { 1 } else { printed }
}

fn git_mirror_json(
    outcome: &crate::providers::redmine::model::RedmineGitMirrorOutcome,
) -> serde_json::Value {
    serde_json::json!({
        "id": outcome.id,
        "project_id": outcome.project_id,
        "identifier": outcome.identifier,
        "status": outcome.status,
        "remote_url": outcome.remote_url,
        "local_path": outcome.local_path,
        "error": outcome.error,
    })
}
