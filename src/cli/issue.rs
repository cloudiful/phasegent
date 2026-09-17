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
        IssueCommand::Update { .. } => (
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
    // One-shot `--body-file` input (issue 298): read and validate the
    // local file before any provider resolution, project discovery, or
    // network access. A read/validation failure exits with the file
    // preserved.
    let body_input = match crate::body_file::resolve_for_issue(&command) {
        Some(Ok(resolved)) => resolved,
        Some(Err((operation, message))) => {
            return super::structured_error(
                serde_json::json!({"kind":"argument", "operation":operation, "message":message}),
                2,
            );
        }
        None => None,
    };
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
        // Repository-aware discovery with bootstrap fallback (issue 394
        // P2): shared with search so the two branches stay identical.
        // Discovery finds exactly one match we use directly and bypass
        // bootstrap; only NoMatch keeps the automatic bootstrap fallback.
        match super::project_resolution::resolve_redmine_project_for_search_or_create(
            role,
            api_base,
            repository,
            project_id,
            close_status_id,
        ) {
            Ok((project, close)) => (project, close),
            Err(error) => return super::provider_error(error),
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
                // No single-number scope guard (issue 394 P3 decision):
                // upload-attachment is uniformly not_supported (see the
                // supports gate above) and never reaches the network, so no
                // GET/PUT can write cross-project. If the capability is ever
                // re-enabled, guard with verify_redmine_scope_before_write
                // before the upload.
                let _ = number;
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
                // Single-number scope guard (issue 394): Redmine verifies
                // the fetched project before returning data; other
                // providers no-op inside the helper.
                if let Err(error) = super::project_resolution::enforce_redmine_single_number_scope(
                    role,
                    api_base,
                    repository,
                    project_id.as_deref(),
                    close_status_id.as_deref(),
                    provider_kind,
                    &summary,
                    number,
                ) {
                    return super::provider_error(error);
                }
                issue_search::warm_single_summary(&provider, &summary, "issue get");
                super::print_json(&summary)
            }
            Err(error) => super::provider_error(error),
        },
        IssueCommand::GetBatch { numbers } => {
            let (issues, mut errors) = batch_fetch_issues(&provider, &numbers);
            // Single-number scope guard per item (issue 394): Redmine
            // partitions guard failures into the per-number `errors`
            // envelope instead of failing the whole batch; other
            // providers no-op. Only passing summaries are warmed.
            let mut guarded = Vec::with_capacity(issues.len());
            if provider_kind == ProviderKind::Redmine {
                let expected = match super::project_resolution::resolve_expected_redmine_project(
                    role,
                    api_base,
                    repository,
                    project_id.as_deref(),
                    close_status_id.as_deref(),
                ) {
                    Ok(expected) => Some(expected),
                    Err(error) => {
                        for summary in &issues {
                            errors.push(serde_json::json!({
                                "number": summary.number,
                                "error": error.json(),
                            }));
                        }
                        return super::print_json(
                            &serde_json::json!({"issues": Vec::<IssueSummary>::new(), "errors": errors}),
                        );
                    }
                };
                if let Some(expected) = expected {
                    for summary in issues {
                        match super::project_resolution::guard_issue_summary_project(
                            &summary,
                            &expected,
                            summary.number,
                        ) {
                            Ok(()) => guarded.push(summary),
                            Err(error) => errors.push(serde_json::json!({
                                "number": summary.number,
                                "error": error.json(),
                            })),
                        }
                    }
                }
            } else {
                guarded = issues;
            }
            for summary in &guarded {
                issue_search::warm_single_summary(&provider, summary, "issue get");
            }
            let failed = !errors.is_empty();
            let code = super::print_json(&serde_json::json!({"issues": guarded, "errors": errors}));
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
            body: _,
            body_file: _,
            keep_body_file: _,
            tracker,
            planning,
            assignee,
            branch,
            base,
            session,
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
            let (body, body_file) = match body_input.as_ref() {
                Some((body, body_file)) => (body.as_str(), body_file.as_ref()),
                None => {
                    return super::structured_error(
                        serde_json::json!({
                            "kind":"argument",
                            "operation":"issue create",
                            "message":"--body or --body-file is required"
                        }),
                        2,
                    );
                }
            };
            match crate::providers::redmine::planning::create_issue(
                &provider,
                &title,
                body,
                tracker.as_deref(),
                &planning,
                &assignee,
            ) {
                Ok((summary, assignee_warning)) => {
                    // GitLab-only: a failing `GET /user` degrades the
                    // default self-assignment to an unassigned create and
                    // surfaces a bounded stderr warning; stdout stays the
                    // normal issue JSON.
                    super::report_local_warnings("issue create", assignee_warning);
                    // Explicit `--branch` (issue 452 P2) is Redmine-only like
                    // the legacy auto-bind. Without `--branch` the legacy
                    // current-branch auto-bind runs; with `--branch` the
                    // target branch is created when missing (from `--base`,
                    // default `HEAD`) and bound instead of the current
                    // branch. Never fails the created issue; warnings go to
                    // stderr so stdout JSON stays byte-identical.
                    if provider_kind == ProviderKind::Redmine {
                        match &branch {
                            crate::command::BranchOption::Unset => {
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
                            crate::command::BranchOption::Auto => {
                                let name = crate::lifecycle::branch_name_for_issue(
                                    tracker.as_deref(),
                                    summary.number,
                                );
                                super::report_local_warnings(
                                    "issue create",
                                    crate::lifecycle::ensure_branch_and_bind(
                                        &crate::branch_context::ProcessGitRunner::new(),
                                        summary.number,
                                        &name,
                                        base.as_deref(),
                                        repository,
                                    )
                                    .warning(),
                                );
                            }
                            crate::command::BranchOption::Named(name) => {
                                super::report_local_warnings(
                                    "issue create",
                                    crate::lifecycle::ensure_branch_and_bind(
                                        &crate::branch_context::ProcessGitRunner::new(),
                                        summary.number,
                                        name,
                                        base.as_deref(),
                                        repository,
                                    )
                                    .warning(),
                                );
                            }
                        }
                    } else if !matches!(&branch, crate::command::BranchOption::Unset) {
                        super::report_local_warnings(
                            "issue create",
                            Some(
                                "issue create --branch is Redmine-only; skipping branch creation"
                                    .to_owned(),
                            ),
                        );
                    }
                    // Issue 18: after a successful create and its bind step,
                    // let the shared conflict table decide whether the current
                    // checkout can be reused or a conflict needs an isolated
                    // worktree. Best-effort: the helper never fails the create,
                    // never deletes a branch or worktree, and reports a created
                    // worktree as a bounded stderr warning so the stdout issue
                    // JSON stays byte-identical.
                    super::report_local_warnings(
                        "issue create",
                        crate::worktree::auto_acquire_after_bind(
                            summary.number,
                            session.as_deref(),
                        ),
                    );
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
                    // Phase 2 tool-driven auto (issue 443): a successful
                    // create implies `In Progress` via `auto_route_next`.
                    // Best-effort timer only; failures stay on stderr so
                    // the stdout issue JSON is byte-identical. Forgejo
                    // stays a silent `Skipped` inside the helper.
                    if let Some(target) = crate::lifecycle_auto::auto_route_next(
                        summary.number,
                        crate::lifecycle_auto::ToolSignal::IssueCreated,
                    ) {
                        super::report_local_warnings(
                            "issue create",
                            crate::lifecycle_auto::auto_transition_timer(
                                summary.number,
                                provider_kind,
                                target,
                            )
                            .warning(),
                        );
                    }
                    issue_search::warm_single_summary(&provider, &summary, "issue create");
                    let exit = super::print_json(&summary);
                    // One-shot `--body-file` cleanup (issue 298): delete
                    // after success unless `--keep-body-file` was given.
                    if exit == 0
                        && let Some(body_file) = body_file
                        && let Some(warning) = body_file.cleanup_after_success()
                    {
                        super::report_local_warnings("issue create", Some(warning));
                    }
                    exit
                }
                Err(error) => super::provider_error(error),
            }
        }
        IssueCommand::Update {
            number,
            body: _,
            body_file: _,
            keep_body_file: _,
            tracker,
            planning,
        } => {
            let (body, body_file) = match body_input.as_ref() {
                Some((body, body_file)) => (body.as_str(), body_file.as_ref()),
                None => {
                    return super::structured_error(
                        serde_json::json!({
                            "kind":"argument",
                            "operation":"issue update",
                            "message":"--body or --body-file is required"
                        }),
                        2,
                    );
                }
            };
            // Single-number scope guard (issue 394 P3 pre-write): resolve +
            // GET-check before the PUT so a cross-project number never
            // writes. Other providers no-op inside the helper.
            if let Err(error) = super::project_resolution::verify_redmine_scope_before_write(
                role,
                api_base,
                repository,
                project_id.as_deref(),
                close_status_id.as_deref(),
                provider_kind,
                &provider,
                number,
            ) {
                return super::provider_error(error);
            }
            match crate::providers::redmine::planning::update_body(
                &provider,
                number,
                body,
                tracker.as_deref(),
                &planning,
            ) {
                Ok(summary) => {
                    issue_search::warm_single_summary(&provider, &summary, "issue update");
                    let exit = super::print_json(&summary);
                    if exit == 0
                        && let Some(body_file) = body_file
                        && let Some(warning) = body_file.cleanup_after_success()
                    {
                        super::report_local_warnings("issue update", Some(warning));
                    }
                    exit
                }
                Err(error) => super::provider_error(error),
            }
        }
        IssueCommand::Close {
            number,
            worktree_session,
        } => {
            // Resolve the worktree session before the remote close so a
            // blank or overlong value fails fast without closing the
            // issue. The resolved context scopes the local lease release
            // to this session (issue 305 Task 3); Task 1 only resolves
            // the identity and emits the legacy migration warning.
            let session = match crate::worktree::resolve_session(worktree_session.as_deref()) {
                Ok(context) => context,
                Err(error) => {
                    return super::structured_error(
                        serde_json::json!({
                            "kind": error.kind,
                            "operation": "issue close",
                            "message": error.message,
                        }),
                        2,
                    );
                }
            };
            // Single-number scope guard (issue 394 P3 pre-write): fails
            // before the PUT so a cross-project number never closes
            // remotely and never triggers local side-effects.
            if let Err(error) = super::project_resolution::verify_redmine_scope_before_write(
                role,
                api_base,
                repository,
                project_id.as_deref(),
                close_status_id.as_deref(),
                provider_kind,
                &provider,
                number,
            ) {
                return super::provider_error(error);
            }
            match provider.close_issue(number) {
                Ok(summary) => {
                    // Legacy fallback is never silent: warn on stderr only
                    // so the stdout close document stays byte-identical.
                    super::report_local_warnings("issue close", session.legacy_warning());
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
                        crate::lifecycle_auto::auto_close_issue_timer(number, provider_kind)
                            .warning(),
                    );
                    // Release only the worktree leases this session owns
                    // (issue 305 Task 3). The legacy fallback never guesses
                    // an owner: it is passed as `None` so no lease is
                    // released. The hook runs after the remote close
                    // succeeded, so a failed close never mutates local
                    // lease state, and warnings stay on stderr.
                    let release_session = match session.source {
                        crate::worktree::SessionSource::Explicit
                        | crate::worktree::SessionSource::Environment => Some(session.id.as_str()),
                        crate::worktree::SessionSource::LegacyFallback => None,
                    };
                    let repo_path =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    super::report_local_warnings(
                        "issue close",
                        crate::lifecycle::release_closed_issue_leases(
                            &crate::worktree::ProcessWorktreeRunner::new(),
                            &repo_path,
                            number,
                            release_session,
                        )
                        .warning(),
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
            }
        }
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
