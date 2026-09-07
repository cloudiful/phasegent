use crate::command::IssueCommand;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::ForgejoError;
use crate::providers::{IssueProvider, ProviderKind};

#[path = "issue_search.rs"]
mod issue_search;
#[path = "issue_search_tests.rs"]
#[cfg(test)]
mod issue_search_tests;

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
        IssueCommand::UploadAttachment { .. } => (
            super::required_role(role_value),
            Capability::IssueAttachmentUpload,
        ),
        // Local branch context commands are dispatched before provider
        // resolution and never reach this function.
        IssueCommand::Bind { .. } | IssueCommand::Unbind | IssueCommand::StatusBranch => {
            unreachable!("local branch context commands bypass provider execution")
        }
    };
    if !role.allows(capability) {
        return super::permission_error(role, capability);
    }
    // Ordinary search validates before any provider work so argument
    // errors never trigger stale fallback.
    if let IssueCommand::Search {
        query,
        state,
        page,
        limit,
        all,
        include_body,
    } = &command
    {
        let options = crate::providers::IssueSearchOptions {
            query: query.clone(),
            state: state.clone(),
            page: *page,
            limit: *limit,
            include_body: *include_body,
            all: *all,
        };
        if let Err(error) = options.validate() {
            return super::provider_error(error);
        }
        return issue_search::execute_search_transparent(
            role,
            provider_kind,
            api_base,
            repository,
            project_id,
            close_status_id,
            options,
        );
    }
    let provider_kind = match resolve_kind(role, provider_kind) {
        Ok(provider) => provider,
        Err(error) => return super::provider_error(error),
    };
    // Redmine-only upload-attachment fast path: reject non-Redmine before
    // any file, network, or credential access so Forgejo/GitLab return the
    // structured not-supported result without trying to upload.
    if let IssueCommand::UploadAttachment { .. } = &command {
        if provider_kind != ProviderKind::Redmine {
            return super::provider_error(ForgejoError::not_supported(
                provider_kind.as_str(),
                capability.operation(),
            ));
        }
    }
    let automatic_workflow = provider_kind == ProviderKind::Redmine
        && project_id.is_none()
        && matches!(&command, IssueCommand::Create { .. });
    let (project_id, close_status_id) = if automatic_workflow {
        // Phase 3: try repository-aware discovery first. An explicit
        // project id already won and is not inside this branch. When
        // discovery finds exactly one match we use it directly and
        // bypass bootstrap (no project creation, membership writes, or
        // mirror POST). Multiple matches fail before any issue write
        // with a bounded listing. Any other discovery HTTP/auth/decode
        // error is propagated, not treated as NoMatch. Only NoMatch
        // keeps the existing automatic bootstrap fallback.
        let discovered = match super::project_resolution::resolve_redmine_project(
            role,
            api_base,
            repository,
            project_id,
            close_status_id,
        ) {
            Ok(value) => value,
            Err(error) => return super::provider_error(error),
        };
        if let Some(discovered_id) = discovered {
            (Some(discovered_id), close_status_id.map(str::to_owned))
        } else {
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
        }
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
        IssueCommand::UploadAttachment {
            number,
            path,
            description,
        } => match &provider {
            crate::providers::ProviderDispatcher::Redmine(redmine) => {
                match redmine.upload_attachment(number, &path, description.as_deref()) {
                    Ok(output) => super::print_json(&output),
                    Err(error) => super::provider_error(error),
                }
            }
            _ => super::provider_error(ForgejoError::not_supported(
                provider.kind().as_str(),
                capability.operation(),
            )),
        },
        IssueCommand::Get { number } => match provider.get_issue(number) {
            Ok(summary) => {
                issue_search::warm_single_summary(&provider, &summary, "issue get");
                super::print_json(&summary)
            }
            Err(error) => super::provider_error(error),
        },
        IssueCommand::Search { .. } => {
            unreachable!("transparent search bypassed provider execution")
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
                    issue_search::warm_single_summary(&provider, &summary, "issue create");
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
        } => match crate::providers::redmine::planning::update_body(
            &provider,
            number,
            &body,
            tracker.as_deref(),
            &planning,
        ) {
            Ok(summary) => {
                issue_search::warm_single_summary(&provider, &summary, "issue update-body");
                super::print_json(&summary)
            }
            Err(error) => super::provider_error(error),
        },
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
                // Close upserts the returned closed document.
                issue_search::warm_single_summary(&provider, &summary, "issue close");
                super::print_json(&summary)
            }
            Err(error) => super::provider_error(error),
        },
        IssueCommand::Bind { .. } | IssueCommand::Unbind | IssueCommand::StatusBranch => {
            unreachable!("local branch context commands bypass provider execution")
        }
    }
}
