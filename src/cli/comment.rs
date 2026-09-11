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
        CommentCommand::Get { .. } | CommentCommand::List { .. } => Capability::CommentRead,
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
    // One-shot `--body-file` input (issue 298): after the permission
    // and authorization gates, read and validate the local file before
    // any provider resolution or network access. A read/validation
    // failure exits with the file preserved.
    let body_input = match crate::body_file::resolve_for_comment(&command) {
        Some(Ok(resolved)) => resolved,
        Some(Err((operation, message))) => {
            return super::structured_error(
                serde_json::json!({"kind":"argument", "operation":operation, "message":message}),
                2,
            );
        }
        None => None,
    };
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
        CommentCommand::Create { issue, marker, .. } => {
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
            let (body, body_file) = match body_input.as_ref() {
                Some((body, body_file)) => (body.as_str(), body_file.as_ref()),
                None => {
                    return super::structured_error(
                        serde_json::json!({
                            "kind":"argument",
                            "operation":"comment create",
                            "message":"--body or --body-file is required"
                        }),
                        2,
                    );
                }
            };
            if !body.contains(&marker) {
                return super::structured_error(
                    serde_json::json!({
                        "kind":"argument",
                        "operation":"comment create",
                        "message":"--body or --body-file content must contain --marker"
                    }),
                    2,
                );
            }
            let result = provider.create_comment(issue, body, &marker);
            let exit = super::print_result(result);
            // Default cleanup runs only on success; any failure above
            // kept the file (the deletion helper is a no-op there).
            if exit == 0
                && let Some(body_file) = body_file
                && let Some(warning) = body_file.cleanup_after_success()
            {
                super::report_local_warnings("comment create", Some(warning));
            }
            exit
        }
        CommentCommand::Get { issue, comment } => {
            super::print_result(provider.get_comment(issue, comment))
        }
        CommentCommand::List { issue } => match provider.list_comments(issue) {
            Ok(comments) => {
                super::print_json(&serde_json::json!({"issue": issue, "comments": comments}))
            }
            Err(error) => super::provider_error(error),
        },
        CommentCommand::FindMarker { issue, marker } => {
            super::print_result(provider.find_marker(issue, &marker))
        }
    }
}
