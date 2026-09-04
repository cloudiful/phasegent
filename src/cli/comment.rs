use crate::command::CommentCommand;
use crate::policy::{Capability, Role};
use crate::providers::forgejo::ForgejoError;
use crate::providers::{IssueProvider, ProviderKind};

pub(crate) fn execute_comment(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: CommentCommand,
) -> i32 {
    let role = super::required_role(role_value);
    let capability = match command {
        CommentCommand::Create { .. } => Capability::CommentCreate,
        CommentCommand::Get { .. } => Capability::CommentRead,
        CommentCommand::FindMarker { .. } => Capability::CommentFindMarker,
    };
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    if let CommentCommand::Create { authorized, .. } = &command
        && role != Role::Orchestrator
        && !authorized
    {
        return super::structured_error(
            serde_json::json!({
                "kind":"authorization",
                "operation":"comment create",
                "message":"executor, reviewer, and tester comment creation requires --authorized"
            }),
            2,
        );
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
        CommentCommand::Create {
            issue,
            body,
            marker,
            authorized: _,
        } => {
            if marker.is_empty() {
                return super::structured_error(
                    serde_json::json!({
                        "kind":"argument",
                        "operation":"comment create",
                        "message":"--marker cannot be empty"
                    }),
                    2,
                );
            }
            if !body.contains(&marker) {
                return super::structured_error(
                    serde_json::json!({
                        "kind":"argument",
                        "operation":"comment create",
                        "message":"--body must contain --marker"
                    }),
                    2,
                );
            }
            super::print_result(provider.create_comment(issue, &body, &marker))
        }
        CommentCommand::Get { issue, comment } => {
            super::print_result(provider.get_comment(issue, comment))
        }
        CommentCommand::FindMarker { issue, marker } => {
            super::print_result(provider.find_marker(issue, &marker))
        }
    }
}
