use crate::command::IssueCommand;
use crate::policy::{Capability, Role};
use crate::providers::api::IssueSummary;
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
        IssueCommand::Get { .. } | IssueCommand::GetBatch { .. } => {
            (super::required_role(role_value), Capability::IssueRead)
        }
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
    // Uniform upload-attachment fast path (Phase 1 parity + Phase 4
    // sink): every provider's inherent `supports` reports
    // `IssueAttachmentUpload = false`, so we reject non-Redmine early
    // — before any file, network, or credential access — with the
    // structured not-supported result. The Redmine arm still falls
    // through to `provider.supports(...)` below and short-circuits
    // there, but the early branch keeps the message tight and avoids
    // resolving `provider_for` for a command we already know to
    // reject.
    if let IssueCommand::UploadAttachment { .. } = &command
        && provider_kind != ProviderKind::Redmine
    {
        return super::provider_error(ForgejoError::not_supported(
            provider_kind.as_str(),
            capability.operation(),
        ));
    }
    let automatic_workflow = provider_kind == ProviderKind::Redmine
        && project_id.is_none()
        && matches!(&command, IssueCommand::Create { .. });
    let (project_id, close_status_id) = if automatic_workflow {
        // Try repository-aware discovery first. An explicit project id
        // already won and is not inside this branch. When discovery
        // finds exactly one match we use it directly and bypass bootstrap
        // (no project creation, membership writes, or mirror POST).
        // Multiple matches fail before any issue write with a bounded
        // listing. Any other discovery HTTP/auth/decode error is
        // propagated, not treated as NoMatch. Only NoMatch keeps the
        // existing automatic bootstrap fallback.
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
        IssueCommand::GetBatch { numbers } => {
            let (issues, errors) = batch_fetch_issues(&provider, &numbers);
            for summary in &issues {
                issue_search::warm_single_summary(&provider, summary, "issue get");
            }
            let failed = !errors.is_empty();
            let code = super::print_json(&serde_json::json!({"issues": issues, "errors": errors}));
            if code != 0 {
                code
            } else if failed {
                1
            } else {
                0
            }
        }
        IssueCommand::Search { .. } => {
            unreachable!("transparent search bypassed provider execution")
        }
        IssueCommand::Create {
            title,
            body,
            tracker,
            planning,
        } => {
            // Phase 3 write-side relation auto (issue 257): the
            // auto-relation fires ONLY on the parent-child split
            // path, so we resolve the planning up front here, keep
            // the freshly validated `parent_issue_id` for the hook
            // below, and pass the original `PlanningOptions` to
            // `planning::create_issue` so the create payload stays
            // byte-identical with the pre-Phase-3 path. The cost
            // of resolving twice (once here, once inside
            // `planning::create_issue`) is one extra
            // GET /versions.json only when `--fixed-version` is
            // supplied alongside `--parent-issue`, which is the
            // rare Phase 3 combination; we accept that so the
            // allowlist stays inside `src/cli/issue.rs` /
            // `src/lifecycle_auto.rs` / relation dispatch +
            // tests. AI agents never run `relation create` by
            // hand for the parent-child split.
            let resolved_planning =
                match crate::providers::redmine::planning::resolve_planning(&provider, &planning) {
                    Ok(resolved) => resolved,
                    Err(error) => return super::provider_error(error),
                };
            let parent_issue_id = resolved_planning.parent_issue_id;
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
                    // Phase 3 relation auto: fire the helper
                    // after a successful create when the parent
                    // linkage was supplied. The helper is
                    // idempotent (skips on Forgejo/Local, skips
                    // silently when parent linkage is absent),
                    // so callers that never use `--parent-issue`
                    // stay unaffected. Any failure degrades to a
                    // bounded Warning on stderr so the JSON
                    // contract on stdout is preserved.
                    super::report_local_warnings(
                        "issue create",
                        crate::lifecycle_auto::auto_create_parent_child_relation(
                            &provider,
                            summary.number,
                            parent_issue_id,
                        )
                        .warning(),
                    );
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
                // Auto-accounting side effect: finish any running
                // auto-run for the issue. The helper is gated for
                // Forgejo internally and returns `Noop` so a
                // Forgejo close never mutates the Redmine or
                // GitLab ledger rows; for Redmine and GitLab it
                // finishes every running row for the issue. The
                // branch-context `unbind_closed_issue` above is a
                // Redmine-only sibling helper and is unaffected by
                // this hook.
                super::report_local_warnings(
                    "issue close",
                    crate::lifecycle_auto::auto_close_issue_timer(number, provider_kind).warning(),
                );
                // Phase 3 relation auto: fire the helper after a
                // successful close. The shared issue DTO does not
                // surface the parent linkage without a server
                // fetch so the call site passes `None`; the
                // helper is silent on the common path. The create
                // arm fires the helper with the resolved
                // `parent_issue_id` and is the only branch that
                // actually creates a relation in Phase 3. See
                // Remaining in the audit note for the deferred
                // lookup shape.
                super::report_local_warnings(
                    "issue close",
                    crate::lifecycle_auto::auto_create_parent_child_relation(
                        &provider, number, None,
                    )
                    .warning(),
                );
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

/// Fetch several issues for `issue get` batches. Successes and
/// failures are collected side by side so one missing issue never
/// discards the rest; the caller renders the `{issues, errors}`
/// envelope. Generic over the provider so tests can drive it
/// against a mock server without a full CLI invocation.
pub(crate) fn batch_fetch_issues<P>(
    provider: &P,
    numbers: &[u64],
) -> (Vec<IssueSummary>, Vec<serde_json::Value>)
where
    P: IssueProvider<Error = ForgejoError>,
{
    let mut issues = Vec::with_capacity(numbers.len());
    let mut errors = Vec::new();
    for number in numbers {
        match provider.get_issue(*number) {
            Ok(summary) => issues.push(summary),
            Err(error) => errors.push(serde_json::json!({"number": number, "error": error.json()})),
        }
    }
    (issues, errors)
}
