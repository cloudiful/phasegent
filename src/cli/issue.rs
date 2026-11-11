use crate::command::IssueCommand;
use crate::policy::{CallerContext, Capability, Role};
use crate::providers::api::IssueSummary;
use crate::providers::api::PhasegentError;
use crate::providers::config::resolve_kind;
use crate::providers::{IssueProvider, ProviderKind};

#[path = "issue_search.rs"]
mod issue_search;
// Issue 628 P4: provider-scoped create/link/switch helpers live next to
// the create arm they belong to; `execute_issue` only dispatches.
#[path = "issue/create_branch.rs"]
mod create_branch;
// Issue 552 Phase 2: the reconciliation engine lives next to the issue
// command it belongs to; `execute_issue` only dispatches into it.
#[path = "sync.rs"]
pub(crate) mod sync;

#[allow(dead_code)]
pub(crate) fn execute_issue(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: IssueCommand,
) -> i32 {
    let caller = CallerContext::from_role(role_value);
    execute_issue_with_caller(
        caller,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
        command,
    )
}

pub(crate) fn execute_issue_with_caller(
    caller: CallerContext,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: IssueCommand,
) -> i32 {
    // `issue sync` is local reconciliation, not a provider issue operation:
    // it gates on orchestrator authority (Operator shares it) and resolves
    // its own provider inside the sync module, so it never enters the
    // capability table below. Managed sessions without a role fail closed
    // before any provider or storage access.
    if let IssueCommand::Sync { all, no_clean } = &command {
        if caller.is_managed_without_role() {
            return super::managed_denial("issue sync");
        }
        if !caller.is_orchestrator_authorized() {
            let role = caller.credential_role().expect("agent has a role");
            return crate::cli::worktree::permission_error(role, "issue sync");
        }
        let role = caller
            .credential_role()
            .expect("authorized caller has a credential");
        return sync::execute_sync_with_caller(
            caller,
            role,
            provider_kind,
            api_base,
            repository,
            project_id,
            close_status_id,
            *all,
            *no_clean,
        );
    }
    let capability = match &command {
        IssueCommand::Get { .. } | IssueCommand::GetBatch { .. } => Capability::IssueRead,
        IssueCommand::Search { .. } => Capability::IssueSearch,
        IssueCommand::Create { .. } => Capability::IssueCreate,
        IssueCommand::Update { .. } => Capability::IssueUpdateBody,
        IssueCommand::Close { .. } => Capability::IssueClose,
        IssueCommand::UploadAttachment { .. } => Capability::IssueAttachmentUpload,
        // Local branch context commands are dispatched before provider
        // resolution and never reach this function; `issue sync` is
        // dispatched above for the same reason.
        IssueCommand::Bind { .. }
        | IssueCommand::Unbind { .. }
        | IssueCommand::StatusBranch
        | IssueCommand::Branches { .. }
        | IssueCommand::Sync { .. } => {
            unreachable!("local-only issue commands bypass provider execution")
        }
    };
    if caller.is_managed_without_role() {
        return super::managed_denial(capability.operation());
    }
    if !caller.allows(capability) {
        let role = caller.credential_role().expect("agent has a role");
        return super::permission_error(role, capability);
    }
    let role = caller
        .credential_role()
        .expect("authorized caller has a credential");
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
    let route_context = super::provider_context::ProviderContext::from_parts(
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
    );
    // A batch resolves every number on its own, so the command's shared
    // provider is never built up front: a bound-only batch must not
    // construct an unproven default provider.
    if let IssueCommand::GetBatch { numbers } = &command {
        return execute_issue_batch(role, &route_context, capability, numbers);
    }
    let routed_number = match &command {
        IssueCommand::Get { number }
        | IssueCommand::Update { number, .. }
        | IssueCommand::Close { number, .. } => Some(*number),
        _ => None,
    };
    let routed = match routed_number {
        Some(number) => {
            match super::task_route::resolve_single_number(role, &route_context, number) {
                Ok(route) => route,
                Err(error) => return super::provider_error(error),
            }
        }
        None => None,
    };
    let is_routed = routed.is_some();
    let explicit_provider = provider_kind;
    // The durable identity of the routed task, kept for the directory-scope
    // decisions below: a routed close releases exactly this task's directories,
    // never another instance's same-number task.
    let routed_task = routed.as_ref().map(|route| {
        let key = route.binding.key();
        (
            route.binding.task_id(),
            key.instance().to_owned(),
            key.project().to_owned(),
            key.provider().to_owned(),
        )
    });
    // Read-only preflight before any provider constructor: a conflicting
    // binding refuses without touching the writable store, schema,
    // migrations, credentials or the network.
    if !is_routed
        && let Some(number) = match &command {
            IssueCommand::Update { number, .. }
            | IssueCommand::Close { number, .. }
            | IssueCommand::Get { number } => Some(*number),
            _ => None,
        }
        && let Err(error) = super::scope_guard::preflight_issue_scope(role, &route_context, number)
    {
        return super::provider_error(error);
    }
    let (provider, provider_kind, project_id, close_status_id) = if let Some(route) = routed {
        let key = route.binding.key();
        (
            route.provider,
            key.provider()
                .parse::<ProviderKind>()
                .expect("validated task provider"),
            Some(key.project().to_owned()),
            close_status_id.map(str::to_owned),
        )
    } else {
        let provider_kind = match resolve_kind(role, provider_kind) {
            Ok(provider) => provider,
            Err(error) => return super::provider_error(error),
        };
        // Uniform upload-attachment fast path: every provider's inherent `supports`
        // reports `IssueAttachmentUpload = false`, so non-Redmine is rejected early
        // — before any file, network, or credential access — with the structured
        // not-supported result. The Redmine arm still falls through to
        // `provider.supports(...)` below and short-circuits there, but the early
        // branch keeps the message tight and avoids resolving `provider_for` for a
        // command we already know to reject.
        if let IssueCommand::UploadAttachment { .. } = &command
            && provider_kind != ProviderKind::Redmine
        {
            return super::provider_error(PhasegentError::not_supported(
                provider_kind.as_str(),
                capability.operation(),
            ));
        }
        // Project discovery is a provider capability (Redmine), not a
        // provider-kind admission rule.
        let automatic_workflow = super::provider_context::capabilities_for(provider_kind)
            .project_discovery
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
            explicit_provider,
            api_base,
            repository,
            project_id.as_deref(),
            close_status_id.as_deref(),
        ) {
            Ok(provider) => provider,
            Err(error) => return super::provider_error(error),
        };
        (provider, provider_kind, project_id, close_status_id)
    };
    // Durable pre-HTTP scope guard (issue 1201 P3): a numbered Forgejo
    // binding is never mutated or read through an omitted or conflicting
    // provider. The read-only local check runs before any provider request,
    // and legacy non-repository checkouts are unaffected.
    match &command {
        IssueCommand::Update { number, .. }
        | IssueCommand::Close { number, .. }
        | IssueCommand::Get { number } => {
            if !is_routed
                && let Err(error) = super::scope_guard::verify_issue_scope(&provider, *number)
            {
                return super::provider_error(error);
            }
        }
        // A batch resolves every number on its own, so its scope guard runs
        // per item against that item's own provider instead of against one
        // shared provider.
        IssueCommand::GetBatch { .. } => {}
        _ => {}
    }
    if !provider.supports(capability) {
        return super::provider_error(PhasegentError::not_supported(
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
            _ => super::provider_error(PhasegentError::not_supported(
                provider.kind().as_str(),
                capability.operation(),
            )),
        },
        IssueCommand::Get { number } => match provider.get_issue(number) {
            Ok(summary) => {
                // Single-number scope guard (issue 394): Redmine verifies
                // the fetched project before returning data; other
                // providers no-op inside the helper.
                if !is_routed
                    && let Err(error) =
                        super::project_resolution::enforce_redmine_single_number_scope(
                            role,
                            api_base,
                            repository,
                            project_id.as_deref(),
                            close_status_id.as_deref(),
                            provider_kind,
                            &summary,
                            number,
                        )
                {
                    return super::provider_error(error);
                }
                issue_search::warm_single_summary(&provider, &summary, "issue get");
                super::print_json(&summary)
            }
            Err(error) => super::provider_error(error),
        },
        IssueCommand::GetBatch { numbers } => {
            // Unreachable in practice: the batch resolves before the shared
            // provider is built, so this keeps the dispatch total without
            // ever using the legacy provider.
            execute_issue_batch(role, &route_context, capability, &numbers)
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
                    // `--assignee` is rejected by every provider, so
                    // `assignee_warning` stays `None`; the warning slot is
                    // retained for the create path.
                    super::report_local_warnings("issue create", assignee_warning);
                    // Persist the durable task identity before best-effort
                    // branch/registry work. A local persistence failure never
                    // retries the remote POST and never hides the created
                    // issue; it reports an actionable adoption warning.
                    let mut created_task: Option<crate::task_identity::TaskBinding> = None;
                    match super::task_route::persist_created_issue(&provider, summary.number) {
                        Ok((binding, repo_associated)) => {
                            super::report_local_warnings(
                                "issue create",
                                super::task_route::attach_created_task_to_branch(
                                    &provider,
                                    summary.number,
                                    binding.task_id(),
                                )
                                .err()
                                .map(|error| {
                                    format!(
                                        "created issue {} but could not attach its task identity to the branch link: {}",
                                        summary.number,
                                        crate::lifecycle::bounded(&error)
                                    )
                                }),
                            );
                            if repo_associated {
                                created_task = Some(binding);
                            } else {
                                super::report_local_warnings(
                                    "issue create",
                                    Some(format!(
                                        "created issue {} but no physical repository was identified; the task identity was persisted without repository association and no task directory was acquired",
                                        summary.number
                                    )),
                                );
                            }
                        }
                        Err(error) => super::report_local_warnings(
                            "issue create",
                            Some(format!(
                                "created issue {} but local task identity persistence failed ({}) ;                                  adopt it with `phasegent issue bind {}`",
                                summary.number,
                                crate::lifecycle::bounded(&error),
                                summary.number
                            )),
                        ),
                    }
                    // Explicit `--branch` (issue 452 P2) is Redmine-only
                    // like the create auto-acquire hook; the create/link
                    // helpers live in `create_branch` so this arm stays
                    // dispatch-only.
                    create_branch::report_create_branch_links(&create_branch::CreateBranchLinks {
                        provider: &provider,
                        provider_kind,
                        issue_number: summary.number,
                        tracker: tracker.as_deref(),
                        branch: &branch,
                        base: base.as_deref(),
                        repository,
                        session: session.as_deref(),
                        context: &route_context,
                    });
                    // Issue 18: after a successful create and its branch
                    // link step, let the shared conflict table decide whether
                    // the current checkout can be reused or a conflict needs
                    // an isolated worktree. Best-effort: the helper never
                    // fails the create, never deletes a branch or worktree,
                    // and reports a created worktree as a bounded stderr
                    // warning so the stdout issue JSON stays byte-identical.
                    //
                    // Issue 1201 P7: every provider's create acquires under the
                    // exact durable task scope just persisted — provider,
                    // project, number, task id and stored instance — and never
                    // falls back to an unscoped association. A create whose identity
                    // could not be persisted or associated with this physical
                    // repository skips the acquisition with its existing
                    // bounded warning instead of inventing an owner.
                    let create_scope = created_task.as_ref().and_then(|binding| {
                        let key = binding.key();
                        crate::worktree::WorktreeScope::for_task(
                            key.provider(),
                            key.project(),
                            summary.number,
                            binding.task_id(),
                            key.instance(),
                        )
                        .ok()
                    });
                    if create_scope.is_some() || created_task.is_some() {
                        super::report_local_warnings(
                            "issue create",
                            match &create_scope {
                                Some(scope) => {
                                    let occupancy =
                                        crate::cli::issue_closure::ClosureOccupancy::task_aware(
                                            role,
                                            route_context.clone(),
                                        );
                                    crate::worktree::auto_acquire_after_create_scoped(
                                        summary.number,
                                        session.as_deref(),
                                        Some(scope),
                                        Some(&occupancy),
                                    )
                                }
                                None => Some(format!(
                                    "created issue {} but its durable task scope could not be \
                                     constructed; skipped task directory acquisition",
                                    summary.number
                                )),
                            },
                        );
                    }
                    // Phase 3 relation auto: fire the helper
                    // after a successful create when the parent
                    // linkage was supplied. The helper is
                    // idempotent (skips on Local, skips
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
                    // the stdout issue JSON is byte-identical. Local
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
            // issue. The attribution stays intentionally optional
            // (issue 575 Phase 1, issue 651 P4): when neither
            // `--worktree-session` nor `PHASEGENT_SESSION_ID` names a
            // session, the close still succeeds and the directories are
            // cleaned without a closing-session move — no owner is ever
            // guessed and the removed literal fallback is gone.
            let session =
                match crate::worktree::resolve_session_optional(worktree_session.as_deref()) {
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
                    // Auto-accounting side effect: finish any running
                    // auto-run for the issue. The helper closes every
                    // running row for the issue (Redmine and Local) and
                    // returns `Noop` when there is nothing to finish.
                    super::report_local_warnings(
                        "issue close",
                        crate::lifecycle_auto::auto_close_issue_timer(number, provider_kind)
                            .warning(),
                    );
                    // The close hook runs after the remote close succeeded,
                    // so a failed close never mutates local registry state,
                    // and warnings stay on stderr. A known closer session is
                    // carried into the guarded cleanup; an unknown closer
                    // still cleans every directory the closed issue proves.
                    let release_session = session.as_ref().map(|context| context.id.as_str());
                    let repo_path =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    // Resolve the verified main checkout before cleanup: a
                    // dedicated closing worktree may be removed below, and
                    // restoration must still have a usable main path.
                    let restore_target = crate::lifecycle::restore::resolve_close_target(
                        &crate::worktree::ProcessWorktreeRunner::new(),
                        &repo_path,
                    );
                    let restore_scope =
                        crate::providers::index_store::provider_scope(&provider).ok();
                    // Resolve any unavailable-preparation warning while the
                    // caller's directory still exists; cleanup may remove it.
                    let restore_unavailable_warning = match &restore_target {
                        Ok(_) => None,
                        Err(reason) => restore_scope.as_ref().and_then(|scope| {
                            let storage = crate::infra::storage::Storage::open().ok()?;
                            crate::lifecycle::restore::close_restore_unavailable_warning(
                                &storage,
                                &repo_path,
                                Some(&scope.source),
                                Some(&scope.project),
                                number,
                                reason,
                            )
                        }),
                    };
                    // A routed close owns exactly its durable task's scope —
                    // provider, project, number, task id and stored instance —
                    // so a same-number task on another instance keeps its own
                    // directory. Without a durable route the exact
                    // resolved provider scope owns its scoped rows, and a
                    // legacy-capable provider (Redmine, Local) additionally
                    // keeps its unscoped numeric compatibility pass, whose
                    // selector already excludes every scoped row.
                    let exact_close_scope =
                        |provider: &crate::providers::ProviderDispatcher| -> Option<crate::worktree::WorktreeScope> {
                            match &routed_task {
                                Some((task_id, instance, project, source)) => {
                                    crate::worktree::WorktreeScope::for_task(
                                        source,
                                        project,
                                        number,
                                        *task_id,
                                        instance,
                                    )
                                    .ok()
                                }
                                None => crate::providers::index_store::provider_scope(provider)
                                    .ok()
                                    .and_then(|scope| {
                                        crate::worktree::WorktreeScope::new(
                                            &scope.source,
                                            &scope.project,
                                            number,
                                        )
                                        .ok()
                                    }),
                            }
                        };
                    let legacy_numeric_leases =
                        crate::providers::IssueProvider::capabilities(&provider)
                            .legacy_numeric_leases;
                    let scoped_release = exact_close_scope(&provider);
                    // Removing a registered directory is decided by the
                    // cleanup guards below: registration alone never blocks.
                    // After the close, delete the closed issue's clean
                    // worktree directories in this repository. The helper
                    // reuses the `worktree prune --remove` primitives and
                    // adds the close-specific guards (clean, no still-open
                    // foreign association, never the main checkout); a
                    // blocked directory is kept and named on stderr, branches
                    // are never deleted, and the stdout close document stays
                    // byte-identical.
                    //
                    // This close pass may move a `ses_` session (the prune
                    // pass may too, through its own chain). A candidate whose
                    // registered `ses_` session is still hosted there first
                    // asks the host to relocate the idle members of its
                    // session family into the repository's verified main
                    // checkout, then confirms every move with fresh evidence. Anything unproven —
                    // unreachable API, incomplete listing, a move that has
                    // not taken effect — keeps the directory and warns.
                    // The other issues the directory is associated with are
                    // confirmed closed through the scope-aware resolver
                    // before anything may move.
                    // A routed close threads its exact task scope and a
                    // task-aware closure: the closed issue's directories are
                    // filtered by that exact task, and every other associated
                    // number is resolved by durable membership only and read
                    // through its own pinned task. A non-routed close with a
                    // provable resolved scope keeps the single-scope closure.
                    // A legacy-capable provider additionally runs its guarded
                    // unscoped compatibility pass, because its history may
                    // carry NULL scope that the scoped pass cannot prove; a
                    // routed close never runs that pass and stays exact-only.
                    if let Some(scope) = scoped_release.as_ref() {
                        let closure = match &routed_task {
                            Some(_) => {
                                crate::cli::issue_closure::CurrentIssueClosure::new_task_aware(
                                    &repo_path,
                                    role,
                                    crate::cli::provider_context::ProviderContext::from_parts(
                                        explicit_provider,
                                        api_base,
                                        repository,
                                        project_id.as_deref(),
                                        close_status_id.as_deref(),
                                    ),
                                )
                            }
                            None => crate::cli::issue_closure::CurrentIssueClosure::new_scoped(
                                &repo_path,
                                role,
                                crate::cli::issue_closure::ExplicitSelection {
                                    provider_kind,
                                    api_base: api_base.map(str::to_owned),
                                    repository: repository.map(str::to_owned),
                                    project_id: project_id.clone(),
                                    close_status_id: close_status_id.clone(),
                                },
                            ),
                        };
                        for warning in crate::lifecycle::cleanup_closed_issue_worktrees_scoped(
                            &crate::worktree::ProcessWorktreeRunner::new(),
                            &repo_path,
                            number,
                            release_session,
                            &closure,
                            scope,
                        )
                        .warnings()
                        {
                            super::report_local_warnings("issue close", Some(warning));
                        }
                    }
                    // The guarded legacy pass only covers history without a
                    // durable task; it never runs for a routed close, whose
                    // exact scope is the whole ownership claim.
                    if legacy_numeric_leases && routed_task.is_none() {
                        let closure = crate::cli::issue_closure::CurrentIssueClosure::new_context(
                            &repo_path,
                            role,
                            crate::cli::provider_context::ProviderContext::from_parts(
                                explicit_provider,
                                api_base,
                                repository,
                                project_id.as_deref(),
                                close_status_id.as_deref(),
                            ),
                        );
                        for warning in crate::lifecycle::cleanup_closed_issue_worktrees(
                            &crate::worktree::ProcessWorktreeRunner::new(),
                            &repo_path,
                            number,
                            release_session,
                            &closure,
                        )
                        .warnings()
                        {
                            super::report_local_warnings("issue close", Some(warning));
                        }
                    }
                    if scoped_release.is_none() && !legacy_numeric_leases {
                        super::report_local_warnings(
                            "issue close",
                            Some(format!(
                                "issue {number} closed; scoped worktree cleanup skipped because the exact provider scope could not be resolved"
                            )),
                        );
                    }
                    // Restoration is best-effort after close and cleanup; its
                    // warnings never change the successful close result.
                    if let Some(warning) = &restore_unavailable_warning {
                        super::report_local_warnings("issue close", Some(warning.clone()));
                    }
                    if let Ok(target) = &restore_target {
                        match crate::infra::storage::Storage::open() {
                            Ok(storage) => {
                                // A recorded baseline carries no instance or
                                // task id, so it can only be attributed when
                                // exactly one durable task in this repository
                                // matches the provider, project and number.
                                // An ambiguous or unprovable membership skips
                                // the restore instead of guessing a baseline.
                                let ambiguous =
                                    crate::lifecycle::restore::ambiguous_restore_warning(
                                        &storage,
                                        &repo_path,
                                        restore_scope.as_ref().map(|scope| scope.source.as_str()),
                                        restore_scope.as_ref().map(|scope| scope.project.as_str()),
                                        number,
                                        routed_task.as_ref().map(|(id, _, _, _)| *id),
                                    );
                                if let Some(warning) = ambiguous {
                                    super::report_local_warnings("issue close", Some(warning));
                                } else {
                                    let git = crate::git_runner::ProcessGitRunner::in_directory(
                                        &target.main_checkout,
                                    );
                                    let api = crate::worktree::opencode::ProcessOpenCodeApi::new();
                                    let runner = crate::worktree::ProcessWorktreeRunner::new();
                                    for warning in crate::lifecycle::restore::restore_on_close(
                                        &crate::lifecycle::restore::CloseRestoreParams {
                                            storage: &storage,
                                            git: &git,
                                            worktree: &runner,
                                            api: &api,
                                            target,
                                            provider: restore_scope
                                                .as_ref()
                                                .map(|scope| scope.source.as_str()),
                                            project: restore_scope
                                                .as_ref()
                                                .map(|scope| scope.project.as_str()),
                                            issue_number: number,
                                            session: release_session,
                                        },
                                    )
                                    .warnings()
                                    {
                                        super::report_local_warnings("issue close", Some(warning));
                                    }
                                }
                            }
                            Err(error) => super::report_local_warnings(
                                "issue close",
                                Some(format!(
                                    "issue {number} closed; checkout restoration skipped because \
                                     the baseline store could not be opened: {error}"
                                )),
                            ),
                        }
                    }
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
                    issue_search::warm_single_summary(&provider, &summary, "issue close");
                    super::print_json(&summary)
                }
                Err(error) => super::provider_error(error),
            }
        }
        IssueCommand::Bind { .. }
        | IssueCommand::Unbind { .. }
        | IssueCommand::StatusBranch
        | IssueCommand::Branches { .. }
        | IssueCommand::Sync { .. } => {
            unreachable!("local-only issue commands bypass provider execution")
        }
    }
}

/// Fetch a batch of issues, resolving every number on its own.
///
/// The command's shared provider is never built up front: a bound-only
/// batch must not construct an unproven default provider. Every requested
/// number is resolved read-only first, the legacy provider is built only
/// when an item is truly unbound, and one item's ambiguity, corruption or
/// provenance failure becomes an isolated error entry instead of
/// preventing the readable items.
fn execute_issue_batch(
    role: Role,
    context: &super::provider_context::ProviderContext,
    capability: Capability,
    numbers: &[u64],
) -> i32 {
    struct Item {
        number: u64,
        route: Option<super::task_route::ResolvedTaskRoute>,
        error: Option<PhasegentError>,
    }
    let items: Vec<Item> = numbers
        .iter()
        .map(
            |number| match super::task_route::resolve_single_number(role, context, *number) {
                Ok(route) => Item {
                    number: *number,
                    route,
                    error: None,
                },
                Err(error) => Item {
                    number: *number,
                    route: None,
                    error: Some(error),
                },
            },
        )
        .collect();
    // Every unbound item is admitted read-only before the legacy provider is
    // constructed; a refusal becomes that item's isolated error and never
    // forces a writable store access for the batch.
    let mut items = items;
    for item in items.iter_mut() {
        if item.error.is_none()
            && item.route.is_none()
            && let Err(error) =
                super::scope_guard::preflight_issue_scope(role, context, item.number)
        {
            item.error = Some(error);
        }
    }
    let has_bound = items.iter().any(|item| item.route.is_some());
    let needs_legacy = items
        .iter()
        .any(|item| item.error.is_none() && item.route.is_none());
    // The legacy provider is built only when an item is truly unbound. A
    // batch without any bound item keeps the legacy failure shape; an
    // unbuildable default never prevents the bound items.
    let mut legacy_error = None;
    let legacy = if needs_legacy {
        match context.resolve(role) {
            Ok(provider) => Some(provider),
            Err(error) if has_bound => {
                legacy_error = Some(error);
                None
            }
            Err(error) => return super::provider_error(error),
        }
    } else {
        None
    };
    let mut issues: Vec<IssueSummary> = Vec::with_capacity(numbers.len());
    let mut errors: Vec<serde_json::Value> = Vec::new();
    for item in &items {
        if let Some(error) = &item.error {
            errors.push(serde_json::json!({"number": item.number, "error": error.json()}));
            continue;
        }
        let (item_provider, expected_project) = match &item.route {
            Some(route) => {
                let key = route.binding.key();
                let expected = if key.provider() == ProviderKind::Redmine.as_str() {
                    Some(key.project().to_owned())
                } else {
                    None
                };
                (&route.provider, expected)
            }
            None => match &legacy {
                Some(provider) => (provider, None),
                None => {
                    let error = legacy_error
                        .as_ref()
                        .expect("an unbound item has a bounded legacy failure");
                    errors.push(serde_json::json!({"number": item.number, "error": error.json()}));
                    continue;
                }
            },
        };
        if item.route.is_none()
            && let Err(error) = super::scope_guard::verify_issue_scope(item_provider, item.number)
        {
            errors.push(serde_json::json!({"number": item.number, "error": error.json()}));
            continue;
        }
        if !item_provider.supports(capability) {
            errors.push(serde_json::json!({
                "number": item.number,
                "error": PhasegentError::not_supported(
                    item_provider.kind().as_str(),
                    capability.operation(),
                )
                .json(),
            }));
            continue;
        }
        let summary = match item_provider.get_issue(item.number) {
            Ok(summary) => summary,
            Err(error) => {
                errors.push(serde_json::json!({"number": item.number, "error": error.json()}));
                continue;
            }
        };
        // A bound item carries its own project in its durable identity; an
        // unbound Redmine item resolves the legacy expected project.
        let expected = match expected_project {
            Some(expected) => Some(expected),
            None if item_provider.kind() == ProviderKind::Redmine => {
                match super::project_resolution::resolve_expected_redmine_project(
                    role,
                    context.api_base.as_deref(),
                    context.repository.as_deref(),
                    context.project_id.as_deref(),
                    context.close_status_id.as_deref(),
                ) {
                    Ok(expected) => Some(expected),
                    Err(error) => {
                        errors.push(
                            serde_json::json!({"number": item.number, "error": error.json()}),
                        );
                        continue;
                    }
                }
            }
            None => None,
        };
        if let Some(expected) = expected
            && let Err(error) = super::project_resolution::guard_issue_summary_project(
                &summary,
                &expected,
                summary.number,
            )
        {
            errors.push(serde_json::json!({"number": item.number, "error": error.json()}));
            continue;
        }
        // The cache write uses the item's own provider; the cache never
        // grants ownership.
        issue_search::warm_single_summary(item_provider, &summary, "issue get");
        issues.push(summary);
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

/// Fetch several issues for `issue get` batches. Successes and
/// failures are collected side by side so one missing issue never
/// discards the rest; the caller renders the `{issues, errors}`
/// envelope. Generic over the provider so tests can drive it
/// against a mock server without a full CLI invocation.
/// Fetch several issues from one provider into the batch envelope.
///
/// The production batch resolves every number on its own, so a bound item
/// is served by its own pinned provider; this shared helper stays for the
/// Redmine provider contract coverage of the per-item error envelope.
#[cfg(test)]
pub(crate) fn batch_fetch_issues<P>(
    provider: &P,
    numbers: &[u64],
) -> (Vec<IssueSummary>, Vec<serde_json::Value>)
where
    P: IssueProvider<Error = PhasegentError>,
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
