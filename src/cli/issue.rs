use crate::command::IssueCommand;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::{IssueProvider, ProviderKind};

pub(crate) fn execute_issue(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: IssueCommand,
) -> i32 {
    let (role, capability) = match &command {
        IssueCommand::Get { .. } => (super::required_role(role_value), Capability::IssueRead),
        IssueCommand::Search { .. } => (super::required_role(role_value), Capability::IssueSearch),
        IssueCommand::Create { .. } => (super::required_role(role_value), Capability::IssueCreate),
        IssueCommand::UpdateBody { .. } => (
            super::required_role(role_value),
            Capability::IssueUpdateBody,
        ),
        IssueCommand::Close { .. } => (super::required_role(role_value), Capability::IssueClose),
        // Local branch context commands are dispatched before provider
        // resolution and never reach this function.
        IssueCommand::Bind { .. } | IssueCommand::Unbind | IssueCommand::StatusBranch => {
            unreachable!("local branch context commands bypass provider execution")
        }
    };
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    let provider_kind = match resolve_kind(role, provider_kind) {
        Ok(provider) => provider,
        Err(error) => return super::provider_error(error),
    };
    let automatic_workflow = provider_kind == ProviderKind::Redmine
        && project_id.is_none()
        && matches!(
            &command,
            IssueCommand::Search { .. } | IssueCommand::Create { .. }
        );
    let (project_id, close_status_id) = if automatic_workflow {
        let state = match crate::workflow::ensure_issue_workflow(
            role,
            api_base,
            repository,
            close_status_id,
        ) {
            Ok(state) => state,
            Err(error) => return super::provider_error(error),
        };
        (
            Some(state.project_id),
            Some(state.close_status_id.to_string()),
        )
    } else {
        (
            project_id.map(str::to_owned),
            close_status_id.map(str::to_owned),
        )
    };
    let provider = match super::provider_for(
        role,
        Some(provider_kind),
        api_base,
        repository,
        project_id.as_deref(),
        close_status_id.as_deref(),
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
        IssueCommand::Get { number } => super::print_result(provider.get_issue(number)),
        IssueCommand::Search { query, state } => {
            super::print_result(provider.search_issues(query.as_deref(), &state))
        }
        IssueCommand::Create {
            title,
            body,
            tracker,
            planning,
        } => {
            match crate::providers::redmine::planning::create_issue(
                &provider,
                &title,
                &body,
                tracker.as_deref(),
                &planning,
            ) {
                Ok(summary) => {
                    // Redmine-only local side effect: bind the new issue to the
                    // current branch when the checkout matches. Never fails the
                    // created issue; warnings go to stderr.
                    if provider_kind == ProviderKind::Redmine {
                        super::report_local_warnings(
                            "issue create",
                            crate::lifecycle::bind_created_issue(
                                &crate::branch_context::ProcessGitRunner::new(),
                                summary.number,
                                repository,
                            )
                            .warning(),
                        );
                    }
                    super::print_json(&summary)
                }
                Err(error) => super::provider_error(error),
            }
        }
        IssueCommand::UpdateBody {
            number,
            body,
            tracker,
            planning,
        } => super::print_result(crate::providers::redmine::planning::update_body(
            &provider,
            number,
            &body,
            tracker.as_deref(),
            &planning,
        )),
        IssueCommand::Close { number } => match provider.close_issue(number) {
            Ok(summary) => {
                // Redmine-only local side effect: unbind only when the current
                // branch points at exactly the closed issue. A failed local
                // unbind never undoes the remote close; warnings go to stderr.
                if provider_kind == ProviderKind::Redmine {
                    super::report_local_warnings(
                        "issue close",
                        crate::lifecycle::unbind_closed_issue(
                            &crate::branch_context::ProcessGitRunner::new(),
                            number,
                            repository,
                        )
                        .warning(),
                    );
                }
                super::print_json(&summary)
            }
            Err(error) => super::provider_error(error),
        },
        IssueCommand::Bind { .. } | IssueCommand::Unbind | IssueCommand::StatusBranch => {
            unreachable!("local branch context commands bypass provider execution")
        }
    }
}
