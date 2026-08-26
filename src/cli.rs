use crate::auth;
use crate::command::{
    self, Command, CommentCommand, HelpTopic, HooksCommand, Invocation, IssueCommand,
    ProjectCommand, RelationCommand, StatusCommand, VersionCommand, WorkflowCommand,
};
use crate::forgejo::{ForgejoConfig, ForgejoError};
use crate::policy::{Capability, Role};
use crate::provider::{
    GitlabConfig, IssueProvider, ProviderDispatcher, ProviderKind, RedmineConfig,
    RedmineMetadataProvider, RedmineProvider,
};
use crate::provider_config::resolve_kind;
use crate::storage::Storage;
use crate::workflow;
use serde::Serialize;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(args: impl IntoIterator<Item = String>) -> i32 {
    match command::parse(&args.into_iter().collect::<Vec<_>>()) {
        Ok(invocation) => execute(invocation),
        Err(message) => usage_error(&message),
    }
}

/// Open the operator's platform-standard SQLite database. CLI entry
/// points that touch [`Storage`] use this helper so the structured
/// error path stays uniform: callers receive the same error string
/// whether the directory could not be resolved or the schema could
/// not be initialised.
fn open_storage() -> Result<Storage, String> {
    Storage::open()
}

fn execute(invocation: Invocation) -> i32 {
    match invocation.command {
        Command::Help(topic) => {
            print_help(invocation.role, invocation.provider, topic);
            0
        }
        Command::Version => {
            println!("phasegent {VERSION}");
            0
        }
        Command::AuthSetup {
            read_stdin,
            provider: auth_provider,
            api_base: auth_base,
            repository: auth_repository,
            project_id: auth_project_id,
            close_status_id: auth_close_status_id,
        } => {
            let role = required_role(invocation.role);
            let requested_provider = auth_provider.or(invocation.provider);
            let provider = match resolve_kind(role, requested_provider) {
                Ok(provider) => provider,
                Err(error) => return provider_error(error),
            };
            match auth::setup_provider(
                role,
                provider.as_str(),
                auth::SetupOptions {
                    read_stdin,
                    api_base: auth_base.or(invocation.api_base),
                    repository: auth_repository.or(invocation.repository),
                    project_id: auth_project_id.or(invocation.project_id),
                    close_status_id: auth_close_status_id.or(invocation.close_status_id),
                },
            ) {
                Ok(value) => print_json(&value),
                Err(message) => {
                    structured_error(serde_json::json!({"kind":"auth", "message":message}), 2)
                }
            }
        }
        Command::ConfigShow => {
            // The CLI re-uses `invocation.role` so a user that runs
            // `phasegent --role executor config show` gets a
            // single-role view, and `phasegent config show` (no
            // role) returns the global snapshot. Open the SQLite
            // database once so any structured failure surfaces
            // through the same path as the config facade.
            let result = open_storage()
                .and_then(|storage| crate::config::show_json(invocation.role, &storage));
            match result {
                Ok(value) => print_json(&value),
                Err(message) => {
                    structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
                }
            }
        }
        Command::ConfigImportEnv => {
            let role = required_role(invocation.role);
            let result =
                open_storage().and_then(|storage| crate::config::import_env_json(role, &storage));
            match result {
                Ok(value) => print_json(&value),
                Err(message) => {
                    structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
                }
            }
        }
        Command::ConfigProviderGet => {
            let result = open_storage().and_then(|storage| crate::config::provider_get(&storage));
            match result {
                Ok(value) => print_json(&value),
                Err(message) => {
                    structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
                }
            }
        }
        Command::ConfigProviderSet { value } => {
            let result = open_storage()
                .and_then(|storage| crate::config::provider_set(value.as_str(), &storage));
            match result {
                Ok(outcome) => print_json(&outcome),
                Err(message) => {
                    structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
                }
            }
        }
        Command::ConfigProviderClear => {
            let result = open_storage().and_then(|storage| crate::config::provider_clear(&storage));
            match result {
                Ok(value) => print_json(&value),
                Err(message) => {
                    structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
                }
            }
        }
        // Local branch context commands bypass provider resolution
        // entirely: they only touch the checkout's own Git config.
        Command::Issue(
            command @ (IssueCommand::Bind { .. }
            | IssueCommand::Unbind
            | IssueCommand::StatusBranch),
        ) => execute_branch_context(command),
        Command::Issue(command) => execute_issue(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Comment(command) => execute_comment(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Project(command) => execute_project(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Status(command) => execute_status(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::VersionCommand(command) => execute_version(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Workflow(command) => execute_workflow(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.close_status_id.as_deref(),
            invocation.close_status_name.as_deref(),
            command,
        ),
        Command::Repo(command) => execute_repo_or_gitlab(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Relation(command) => execute_relation(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Timer(command) => crate::time_tracking_cli::execute(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        )
        .map_or_else(crate::cli::provider_error, |output| print_json(&output)),
        Command::Ci(command) => execute_ci_or_gitlab(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Hooks(command) => execute_hooks(command),
    }
}

fn execute_branch_context(command: IssueCommand) -> i32 {
    let runner = crate::branch_context::ProcessGitRunner::new();
    let result = match command {
        IssueCommand::Bind { issue_id, replace } => {
            crate::branch_context::execute_bind(&runner, issue_id, replace)
        }
        IssueCommand::Unbind => crate::branch_context::execute_unbind(&runner),
        IssueCommand::StatusBranch => crate::branch_context::execute_status(&runner),
        _ => unreachable!("branch context dispatch handles only local issue commands"),
    };
    match result {
        Ok(value) => print_json(&value),
        Err(error) => structured_error(error.json(), 1),
    }
}

fn execute_hooks(command: HooksCommand) -> i32 {
    match command {
        HooksCommand::Install => match crate::hooks::install() {
            Ok(outcome) => print_json(&serde_json::json!({
                "installed": outcome.installed,
                "updated": outcome.updated,
                "warnings": outcome.warnings,
            })),
            Err(error) => structured_error(error.json(), 2),
        },
        // Invoked by generated hook scripts; no role, provider, or network.
        HooksCommand::Run {
            hook,
            message_file,
            source,
        } => match crate::hooks::run(hook, &message_file, source.as_deref()) {
            Ok(value) => print_json(&value),
            Err(error) => structured_error(error.json(), 1),
        },
    }
}

fn execute_workflow(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    global_repository: Option<&str>,
    global_close_status_id: Option<&str>,
    global_close_status_name: Option<&str>,
    command: WorkflowCommand,
) -> i32 {
    let role = required_role(role_value);
    if role != Role::Admin {
        return structured_error(
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
        Err(error) => return provider_error(error),
    };
    if provider != ProviderKind::Redmine {
        return provider_error(ForgejoError::not_supported(
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
        return usage_error("workflow bootstrap received a duplicate close-status option");
    }
    let close_status_id = close_status_id.or_else(|| global_close_status_id.map(str::to_owned));
    let close_status_name =
        close_status_name.or_else(|| global_close_status_name.map(str::to_owned));
    if close_status_id.is_some() && close_status_name.is_some() {
        return usage_error(
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
        Err(error) => return provider_error(error),
    };
    if !result.ready() {
        return print_bootstrap_warning(&result);
    }
    // A matching checkout whose hook installation failed still bootstraps
    // successfully; surface the bounded reason on stderr next to the JSON.
    if let Some(hooks) = &result.hooks {
        report_local_warnings("workflow bootstrap", hooks.warning());
    }
    print_json(&bootstrap_success_json(&result))
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
    let printed = print_json(&payload);
    if printed == 0 { 1 } else { printed }
}

fn git_mirror_json(outcome: &crate::redmine_model::RedmineGitMirrorOutcome) -> serde_json::Value {
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

fn execute_issue(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: IssueCommand,
) -> i32 {
    let (role, capability) = match &command {
        IssueCommand::Get { .. } => (required_role(role_value), Capability::IssueRead),
        IssueCommand::Search { .. } => (required_role(role_value), Capability::IssueSearch),
        IssueCommand::Create { .. } => (required_role(role_value), Capability::IssueCreate),
        IssueCommand::UpdateBody { .. } => (required_role(role_value), Capability::IssueUpdateBody),
        IssueCommand::Close { .. } => (required_role(role_value), Capability::IssueClose),
        // Local branch context commands are dispatched before provider
        // resolution and never reach this function.
        IssueCommand::Bind { .. } | IssueCommand::Unbind | IssueCommand::StatusBranch => {
            unreachable!("local branch context commands bypass provider execution")
        }
    };
    if !role.allows(capability) {
        return permission_error(role, capability);
    }
    let provider_kind = match resolve_kind(role, provider_kind) {
        Ok(provider) => provider,
        Err(error) => return provider_error(error),
    };
    let automatic_workflow = provider_kind == ProviderKind::Redmine
        && project_id.is_none()
        && matches!(
            &command,
            IssueCommand::Search { .. } | IssueCommand::Create { .. }
        );
    let (project_id, close_status_id) = if automatic_workflow {
        let state =
            match workflow::ensure_issue_workflow(role, api_base, repository, close_status_id) {
                Ok(state) => state,
                Err(error) => return provider_error(error),
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
    let provider = match provider_for(
        role,
        Some(provider_kind),
        api_base,
        repository,
        project_id.as_deref(),
        close_status_id.as_deref(),
    ) {
        Ok(provider) => provider,
        Err(error) => return provider_error(error),
    };
    if !provider.supports(capability) {
        return provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match command {
        IssueCommand::Get { number } => print_result(provider.get_issue(number)),
        IssueCommand::Search { query, state } => {
            print_result(provider.search_issues(query.as_deref(), &state))
        }
        IssueCommand::Create {
            title,
            body,
            tracker,
            planning,
        } => {
            match crate::redmine_planning_cli::create_issue(
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
                        report_local_warnings(
                            "issue create",
                            crate::lifecycle::bind_created_issue(
                                &crate::branch_context::ProcessGitRunner::new(),
                                summary.number,
                                repository,
                            )
                            .warning(),
                        );
                    }
                    print_json(&summary)
                }
                Err(error) => provider_error(error),
            }
        }
        IssueCommand::UpdateBody {
            number,
            body,
            tracker,
            planning,
        } => print_result(crate::redmine_planning_cli::update_body(
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
                    report_local_warnings(
                        "issue close",
                        crate::lifecycle::unbind_closed_issue(
                            &crate::branch_context::ProcessGitRunner::new(),
                            number,
                            repository,
                        )
                        .warning(),
                    );
                }
                print_json(&summary)
            }
            Err(error) => provider_error(error),
        },
        IssueCommand::Bind { .. } | IssueCommand::Unbind | IssueCommand::StatusBranch => {
            unreachable!("local branch context commands bypass provider execution")
        }
    }
}

/// Emits bounded lifecycle warnings on stderr so stdout JSON stays compatible
/// with the plain provider output shape.
fn report_local_warnings(operation: &str, warnings: Option<String>) {
    if let Some(reason) = warnings {
        eprintln!(
            "{}",
            serde_json::json!({
                "warning": {
                    "operation": operation,
                    "message": reason,
                }
            })
        );
    }
}

fn execute_comment(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: CommentCommand,
) -> i32 {
    let role = required_role(role_value);
    let capability = match command {
        CommentCommand::Create { .. } => Capability::CommentCreate,
        CommentCommand::Get { .. } => Capability::CommentRead,
        CommentCommand::FindMarker { .. } => Capability::CommentFindMarker,
    };
    if !role.allows(capability) {
        return permission_error(role, capability);
    }
    if let CommentCommand::Create { authorized, .. } = &command
        && role != Role::Orchestrator
        && !authorized
    {
        return structured_error(
            serde_json::json!({
                "kind":"authorization",
                "operation":"comment create",
                "message":"executor and reviewer comment creation requires --authorized"
            }),
            2,
        );
    }
    let provider = match provider_for(
        role,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
    ) {
        Ok(provider) => provider,
        Err(error) => return provider_error(error),
    };
    if !provider.supports(capability) {
        return provider_error(ForgejoError::not_supported(
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
                return structured_error(
                    serde_json::json!({
                        "kind":"argument",
                        "operation":"comment create",
                        "message":"--marker cannot be empty"
                    }),
                    2,
                );
            }
            if !body.contains(&marker) {
                return structured_error(
                    serde_json::json!({
                        "kind":"argument",
                        "operation":"comment create",
                        "message":"--body must contain --marker"
                    }),
                    2,
                );
            }
            print_result(provider.create_comment(issue, &body, &marker))
        }
        CommentCommand::Get { issue, comment } => {
            print_result(provider.get_comment(issue, comment))
        }
        CommentCommand::FindMarker { issue, marker } => {
            print_result(provider.find_marker(issue, &marker))
        }
    }
}

fn execute_project(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: ProjectCommand,
) -> i32 {
    let (role, capability) = match &command {
        ProjectCommand::List => (required_role(role_value), Capability::ProjectRead),
        ProjectCommand::Create { .. } => (required_role(role_value), Capability::ProjectCreate),
    };
    if !role.allows(capability) {
        return permission_error(role, capability);
    }
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            return provider_error(ForgejoError::not_supported(
                "forgejo",
                capability.operation(),
            ));
        }
        Ok(ProviderKind::Redmine) => {}
        Ok(ProviderKind::Gitlab) => {
            return provider_error(ForgejoError::not_supported(
                "gitlab",
                capability.operation(),
            ));
        }
        Err(error) => return provider_error(error),
    }
    let provider = match provider_for(
        role,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
    ) {
        Ok(provider) => provider,
        Err(error) => return provider_error(error),
    };
    if !provider.supports(capability) {
        return provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match command {
        ProjectCommand::List => print_result(provider.list_projects()),
        ProjectCommand::Create {
            name,
            identifier,
            description,
            confirmed,
        } => {
            if !confirmed {
                return structured_error(
                    serde_json::json!({
                        "kind":"authorization",
                        "operation":"project create",
                        "message":"project creation requires --confirm"
                    }),
                    2,
                );
            }
            print_result(provider.create_project(&name, &identifier, description.as_deref()))
        }
    }
}

fn execute_status(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: StatusCommand,
) -> i32 {
    let role = required_role(role_value);
    let capability = Capability::IssueStatusRead;
    if !role.allows(capability) {
        return permission_error(role, capability);
    }
    // Status transitions drive the workflow lifecycle and are
    // orchestrator-owned, mirroring issue close; executors, reviewers,
    // and the admin bootstrap identity may not move an issue's status.
    // The check runs before any provider or network access so a denied
    // role fails fast with a structured permission error.
    if matches!(command, StatusCommand::Set { .. }) && role != Role::Orchestrator {
        return structured_error(
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
            return provider_error(ForgejoError::not_supported(
                "forgejo",
                capability.operation(),
            ));
        }
        Ok(ProviderKind::Redmine) => {}
        // GitLab: list is unsupported (no native status enum), but
        // `set` maps to a managed workflow label update; the
        // orchestrator-only guard above already protects it.
        Ok(ProviderKind::Gitlab) => {}
        Err(error) => return provider_error(error),
    }
    let provider = match provider_for(
        role,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
    ) {
        Ok(provider) => provider,
        Err(error) => return provider_error(error),
    };
    // GitLab exposes `IssueStatusRead` so the dispatch does not bail
    // out on the capability check; the per-command branch below
    // decides whether the call is supported. Redmine still uses the
    // capability surface for the actual work.
    if !provider.supports(capability) && !matches!(provider, ProviderDispatcher::Gitlab(_)) {
        return provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match command {
        StatusCommand::List => match provider {
            ProviderDispatcher::Gitlab(_) => {
                provider_error(ForgejoError::not_supported("gitlab", "issue status list"))
            }
            _ => print_result(provider.list_issue_statuses()),
        },
        StatusCommand::Set { number, status } => match provider {
            ProviderDispatcher::Gitlab(gitlab) => {
                print_result(gitlab.set_workflow_status(number, &status))
            }
            ProviderDispatcher::Redmine(redmine) => {
                let statuses = match redmine.list_issue_statuses() {
                    Ok(statuses) => statuses,
                    Err(error) => return provider_error(error),
                };
                let target = match RedmineProvider::select_status_by_value(&statuses, &status) {
                    Ok(target) => target,
                    Err(error) => return provider_error(error),
                };
                print_result(redmine.set_issue_status(number, target.id))
            }
            ProviderDispatcher::Forgejo(_) => provider_error(ForgejoError::not_supported(
                "forgejo",
                "issue status update",
            )),
        },
    }
}

/// Redmine or GitLab issue relations. `list` is available to every non-admin
/// role (orchestrator/executor/reviewer), while `create` and `delete` are
/// orchestrator-only; the admin identity is denied all three. Forgejo
/// rejects every relation operation with a structured not-supported error
/// before any network access. Phase 4 lifts the GitLab foundation
/// restriction so the dispatch path also handles GitLab's
/// `/links` endpoint, with `precedes` and `--delay` rejected as
/// structured config errors rather than silently mapped.
fn execute_relation(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: RelationCommand,
) -> i32 {
    let role = required_role(role_value);
    let capability = match &command {
        RelationCommand::List { .. } => Capability::RelationRead,
        RelationCommand::Create { .. } => Capability::RelationCreate,
        RelationCommand::Delete { .. } => Capability::RelationDelete,
    };
    if !role.allows(capability) {
        return permission_error(role, capability);
    }
    // Forgejo has no issue relations; reject before any provider build or
    // network access so the structured not-supported error is the only side
    // effect. Redmine and GitLab both continue; the dispatch layer in
    // `redmine_relations_cli` validates provider-specific flags.
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            return provider_error(ForgejoError::not_supported(
                "forgejo",
                capability.operation(),
            ));
        }
        Ok(ProviderKind::Redmine) | Ok(ProviderKind::Gitlab) => {}
        Err(error) => return provider_error(error),
    }
    let provider = match provider_for(
        role,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
    ) {
        Ok(provider) => provider,
        Err(error) => return provider_error(error),
    };
    if !provider.supports(capability) {
        return provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match crate::redmine_relations_cli::execute(&provider, &command) {
        Ok(crate::redmine_relations_cli::RelationResult::List(relations)) => print_json(&relations),
        Ok(crate::redmine_relations_cli::RelationResult::Created(summary)) => print_json(&summary),
        Ok(crate::redmine_relations_cli::RelationResult::Deleted(relation_id)) => {
            print_json(&serde_json::json!({"deleted": relation_id, "relation_id": relation_id}))
        }
        Err(error) => provider_error(error),
    }
}

/// Redmine-only project version discovery. Every role may read versions
/// (planning is read-mostly), while Forgejo rejects the operation with a
/// structured not-supported error before any network access.
fn execute_version(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: VersionCommand,
) -> i32 {
    let role = required_role(role_value);
    let capability = Capability::VersionRead;
    if !role.allows(capability) {
        return permission_error(role, capability);
    }
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            return provider_error(ForgejoError::not_supported(
                "forgejo",
                capability.operation(),
            ));
        }
        Ok(ProviderKind::Redmine) => {}
        Ok(ProviderKind::Gitlab) => {
            return provider_error(ForgejoError::not_supported(
                "gitlab",
                capability.operation(),
            ));
        }
        Err(error) => return provider_error(error),
    }
    let provider = match provider_for(
        role,
        provider_kind,
        api_base,
        repository,
        project_id,
        close_status_id,
    ) {
        Ok(provider) => provider,
        Err(error) => return provider_error(error),
    };
    if !provider.supports(capability) {
        return provider_error(ForgejoError::not_supported(
            provider.kind().as_str(),
            capability.operation(),
        ));
    }
    match command {
        VersionCommand::List => print_result(provider.list_project_versions()),
    }
}

pub(crate) fn provider(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
) -> Result<ProviderDispatcher, ForgejoError> {
    provider_for(
        role,
        Some(ProviderKind::Forgejo),
        api_base,
        repository,
        None,
        None,
    )
}

fn provider_for(
    role: Role,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<ProviderDispatcher, ForgejoError> {
    match resolve_kind(role, provider_kind)? {
        ProviderKind::Forgejo => {
            let config = ForgejoConfig::resolve(role, api_base, repository)?;
            match config.provider() {
                ProviderKind::Forgejo => ProviderDispatcher::for_role(role, config),
                ProviderKind::Redmine => Err(ForgejoError::config(
                    "Forgejo configuration selected an unsupported provider",
                )),
                ProviderKind::Gitlab => Err(ForgejoError::config(
                    "Forgejo configuration selected an unsupported provider",
                )),
            }
        }
        ProviderKind::Redmine => {
            let config = RedmineConfig::resolve(role, api_base, project_id, close_status_id)?;
            ProviderDispatcher::redmine(role, config)
        }
        ProviderKind::Gitlab => {
            // The CLI shares the Redmine flag namespace for the project
            // id; the GitLab resolver is numeric and rejects a Redmine
            // close status id or a Forgejo repository. The dispatcher
            // still hands the resolved config to GitlabProvider so the
            // not-supported stubs in this foundation phase receive the
            // exact URL and project id the caller asked for.
            let _ = repository;
            let _ = close_status_id;
            let config = GitlabConfig::resolve(role, api_base, project_id)?;
            ProviderDispatcher::gitlab(role, config)
        }
    }
}

/// Route `repo create` to either Forgejo or GitLab. Phase 3 lifts
/// the Forgejo-only restriction so GitLab repository creation can
/// reach the GitLab provider; Redmine still rejects the operation
/// because it has no first-class repository endpoint.
fn execute_repo_or_gitlab(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: crate::command::RepoCommand,
) -> i32 {
    let role = required_role(role_value);
    let capability = Capability::RepoCreate;
    if !role.allows(capability) {
        return permission_error(role, capability);
    }
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            crate::repo_cli::execute(role_value, api_base, repository, command)
        }
        Ok(ProviderKind::Gitlab) => match provider_for(
            role,
            Some(ProviderKind::Gitlab),
            api_base,
            repository,
            project_id,
            close_status_id,
        ) {
            Ok(provider) => {
                print_result(provider.create_repo_for_command(&command, role, api_base, repository))
            }
            Err(error) => provider_error(error),
        },
        Ok(ProviderKind::Redmine) => {
            provider_error(ForgejoError::not_supported("redmine", "repo create"))
        }
        Err(error) => provider_error(error),
    }
}

/// Route `ci` reads to either Forgejo or GitLab. Phase 3 lifts the
/// Forgejo-only restriction so GitLab CI reads can reach the GitLab
/// provider; Redmine still rejects because it has no first-class CI
/// endpoint.
fn execute_ci_or_gitlab(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: crate::command::CiCommand,
) -> i32 {
    let role = required_role(role_value);
    let capability = Capability::CiRead;
    if !role.allows(capability) {
        return permission_error(role, capability);
    }
    match resolve_kind(role, provider_kind) {
        Ok(ProviderKind::Forgejo) => {
            crate::ci_cli::execute(role_value, api_base, repository, command)
        }
        Ok(ProviderKind::Gitlab) => match provider_for(
            role,
            Some(ProviderKind::Gitlab),
            api_base,
            repository,
            project_id,
            close_status_id,
        ) {
            Ok(provider) => print_result(provider.ci_for_command(&command)),
            Err(error) => provider_error(error),
        },
        Ok(ProviderKind::Redmine) => {
            provider_error(ForgejoError::not_supported("redmine", "ci read"))
        }
        Err(error) => provider_error(error),
    }
}

fn print_help(role: Option<Role>, provider: Option<ProviderKind>, topic: HelpTopic) {
    match topic {
        HelpTopic::Root => print_root_help(role, provider),
        HelpTopic::Issue => print_issue_help(role),
        HelpTopic::Comment => print_comment_help(role),
        HelpTopic::Project => print_project_help(role),
        HelpTopic::Status => print_status_help(role),
        HelpTopic::Version => print_version_help(role),
        HelpTopic::Workflow => print_workflow_help(role),
        HelpTopic::Auth => print_auth_help(role),
        HelpTopic::Config => print_config_help(role),
        HelpTopic::ConfigCommand(command) => print_config_command_help(role, &command),
        HelpTopic::ConfigProvider => print_config_provider_help(),
        HelpTopic::ConfigProviderCommand(command) => print_config_provider_command_help(&command),
        HelpTopic::Repo => {
            if provider == Some(ProviderKind::Redmine) {
                print_not_supported_help("repo")
            } else {
                crate::repo_cli::print_help(role)
            }
        }
        HelpTopic::IssueCommand(command) => print_issue_command_help(role, &command),
        HelpTopic::CommentCommand(command) => print_comment_command_help(role, &command),
        HelpTopic::ProjectCommand(command) => print_project_command_help(role, &command),
        HelpTopic::StatusCommand(command) => print_status_command_help(role, &command),
        HelpTopic::VersionCommand(command) => print_version_command_help(role, &command),
        HelpTopic::WorkflowCommand(command) => print_workflow_command_help(role, &command),
        HelpTopic::Relation => print_relation_help(role),
        HelpTopic::RelationCommand(command) => print_relation_command_help(role, &command),
        HelpTopic::Timer => print_timer_help(role),
        HelpTopic::TimerCommand(command) => print_timer_command_help(role, &command),
        HelpTopic::RepoCommand(command) => {
            if provider == Some(ProviderKind::Redmine) {
                print_not_supported_help(&format!("repo {command}"))
            } else {
                crate::repo_cli::print_command_help(role, &command, provider)
            }
        }
        HelpTopic::Ci => {
            if provider == Some(ProviderKind::Redmine) {
                print_not_supported_help("ci")
            } else {
                crate::ci_cli::print_help(role)
            }
        }
        HelpTopic::CiCommand(command) => {
            if provider == Some(ProviderKind::Redmine) {
                print_not_supported_help(&format!("ci {command}"))
            } else {
                crate::ci_cli::print_command_help(role, &command)
            }
        }
        HelpTopic::Hooks => print_hooks_help(),
        HelpTopic::HooksCommand(command) => print_hooks_command_help(&command),
    }
}

fn print_root_help(role: Option<Role>, provider: Option<ProviderKind>) {
    let role_text = role.map_or("all roles", Role::as_str);
    println!(
        "phasegent {VERSION}\n\nProvider-backed workflow CLI ({role_text}).\nRole selects a capability policy; it is not an identity boundary.\n\nUsage:\n  phasegent --role <ROLE> [--provider forgejo|redmine|gitlab] <COMMAND> [OPTIONS]\n\nOptions:\n  --role <ROLE>          admin, orchestrator, executor, or reviewer\n  --provider <NAME>      forgejo, redmine, or gitlab (default: forgejo)\n  --api-base <URL>       Override the provider API base\n  --repository <O/R>     Override the Forgejo owner/repository\n  --project-id <ID>      Override the Redmine or GitLab project id\n  --close-status-id <ID> Override the Redmine closed status\n  --close-status-name NAME Select a Redmine closed status during bootstrap\n  -h, --help             Print help\n  -V, --version          Print version\n\nCommands:\n  issue                  Issue operations\n  comment                Comment operations\n  auth                   Authentication setup\n  config                 Local configuration show, import-env, and provider default\n  hooks                  Managed Git hook installation\n\nProvider resolution precedence (highest first):\n  1. explicit --provider\n  2. PHASEGENT_PROVIDER environment variable\n  3. PHASEGENT_DEFAULT_PROVIDER environment variable\n  4. persisted PHASEGENT_DEFAULT_PROVIDER in SQLite (config provider set/get/clear)\n  5. role-scoped role_config.provider\n  6. forgejo fallback\nThe resolver is read-only; --provider is the per-command override and the persisted default is machine-wide."
    );
    if provider != Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::RepoCreate))
    {
        println!("  repo                   Repository operations");
    }
    if provider != Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::CiRead))
    {
        // The same `ci` command is routed to GitLab when the
        // provider is gitlab; Phase 3 only adjusts the description
        // here so the help output does not falsely advertise a
        // Forgejo-only surface.
        let label = match provider {
            Some(ProviderKind::Gitlab) => "GitLab pipeline read operations",
            _ => "Forgejo Actions read operations",
        };
        println!("  ci                     {label}");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::ProjectRead))
    {
        println!("  project                Redmine project operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::IssueStatusRead))
    {
        println!("  status                 Redmine issue status operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::VersionRead))
    {
        println!("  version                Redmine project version operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::RelationRead))
    {
        println!("  relation               Redmine issue relations");
    }
    if provider == Some(ProviderKind::Redmine) && role.is_none_or(|role| role == Role::Orchestrator)
    {
        println!("  timer                  Redmine phase time tracking");
    }
    if provider == Some(ProviderKind::Redmine) && role.is_none_or(|role| role == Role::Admin) {
        println!("  workflow               Redmine workflow bootstrap");
    }
    println!("\nUse 'phasegent --help <command>' for the next level.");
}

fn print_issue_help(role: Option<Role>) {
    println!(
        "Issue commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    for (name, capability) in [
        ("get", Capability::IssueRead),
        ("search", Capability::IssueSearch),
        ("create", Capability::IssueCreate),
        ("update-body", Capability::IssueUpdateBody),
        ("close", Capability::IssueClose),
    ] {
        if role.is_none_or(|role| role.allows(capability)) {
            println!("  {name:<14} {}", capability.description());
        }
    }
    println!("\nLocal branch context (no provider or network access):");
    println!("  bind             Bind the current branch to a Redmine issue in local Git config");
    println!("  unbind           Remove the current branch's Redmine issue binding");
    println!("  status           Show the current branch and its bound Redmine issue, if any");
    println!("\nUse 'phasegent --help issue <command>' for options.");
}

fn print_comment_help(role: Option<Role>) {
    println!(
        "Comment commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    for (name, capability) in [
        ("create", Capability::CommentCreate),
        ("get", Capability::CommentRead),
        ("find-marker", Capability::CommentFindMarker),
    ] {
        if role.is_none_or(|role| role.allows(capability)) {
            println!("  {name:<14} {}", capability.description());
        }
    }
    println!("\nUse 'phasegent --help comment <command>' for options.");
}

fn print_project_help(role: Option<Role>) {
    println!(
        "Project commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| role.allows(Capability::ProjectRead)) {
        println!(
            "  list             {}",
            Capability::ProjectRead.description()
        );
    }
    if role.is_none_or(|role| role.allows(Capability::ProjectCreate)) {
        println!(
            "  create           {}",
            Capability::ProjectCreate.description()
        );
    }
    println!("\nUse 'phasegent --help project <command>' for options.");
}

fn print_status_help(role: Option<Role>) {
    println!(
        "Status commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| role.allows(Capability::IssueStatusRead)) {
        println!(
            "  list             {}",
            Capability::IssueStatusRead.description()
        );
    }
    if role.is_none_or(|role| role == Role::Orchestrator) {
        println!("  set              Update a Redmine issue status by validated name or id");
    }
    println!("\nUse 'phasegent --help status <command>' for options.");
}

fn print_version_help(role: Option<Role>) {
    println!(
        "Version commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| role.allows(Capability::VersionRead)) {
        println!(
            "  list             {}",
            Capability::VersionRead.description()
        );
    }
    println!("\nUse 'phasegent --help version <command>' for options.");
}

fn print_version_command_help(role: Option<Role>, command: &str) {
    if command != "list" {
        print_version_help(role);
        return;
    }
    let capability = Capability::VersionRead;
    if role.is_none_or(|role| role.allows(capability)) {
        println!("Usage: version list\n\n{}", capability.description());
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

fn print_relation_help(role: Option<Role>) {
    println!(
        "Relation commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| role.allows(Capability::RelationRead)) {
        println!(
            "  list              {}",
            Capability::RelationRead.description()
        );
    }
    if role.is_none_or(|role| role == Role::Orchestrator) {
        println!(
            "  create           Create a Redmine or GitLab issue relation (orchestrator-only)"
        );
        println!(
            "  delete           Delete a Redmine or GitLab issue relation by id (orchestrator-only)"
        );
    }
    println!("\nUse 'phasegent --help relation <command>' for options.");
}

fn print_relation_command_help(role: Option<Role>, command: &str) {
    match command {
        "list" => {
            if role.is_none_or(|role| role.allows(Capability::RelationRead)) {
                println!(
                    "Usage: relation list <ISSUE>\n\n{}",
                    Capability::RelationRead.description()
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "create" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: relation create <ISSUE> --to <ISSUE> --type blocks|precedes|relates [--delay N]\n\nCreates an issue relation from <ISSUE> to --to of the given type. `blocks`/`blocked` and `precedes`/`follows` are inverse directions; only the forward canonical names (blocks, precedes, relates) are accepted as --type. `--delay N` (a non-negative integer lag) is only valid with --type precedes. Redmine honours every flag. GitLab currently accepts only --type relates for create; --type blocks and --type precedes are rejected with structured not-supported / config errors before any network traffic, and --delay is rejected as a structured config error. GitLab relation list still maps every server-returned direction (blocks, is_blocked_by) so listing an issue reflects whatever the server already recorded. Forgejo always rejects relation operations. Orchestrator-only."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "delete" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: relation delete <RELATION_ID> [--issue <SOURCE_ISSUE_IID>]\n\nDeletes a Redmine, GitLab, or Forgejo-rejected issue relation by its numeric id. Orchestrator-only. GitLab additionally requires --issue <SOURCE_ISSUE_IID> because the DELETE endpoint is scoped per source issue; Redmine and Forgejo ignore the flag."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        _ => print_relation_help(role),
    }
}

fn print_timer_help(role: Option<Role>) {
    if role.is_none_or(|role| role == Role::Orchestrator) {
        println!(
            "Timer commands for orchestrators:\n\n  start <ISSUE> --phase NAME --agent-role executor|reviewer --attempt N [--run-id ID]    Persist a local phase run\n  finish <RUN_ID> --result DONE|PARTIAL|BLOCKED|FAILED    Finish the run and project its rounded time to Redmine or GitLab\n\nTimer start is local-only and must write the ledger before any projection. Finish is Redmine- or GitLab-only (Forgejo rejects both) and orchestrator-only. Redmine receives the rounded 0.01-hour summary; GitLab receives the exact elapsed seconds in human-format duration (e.g. 1h30m). Exact elapsed seconds remain in SQLite."
        );
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

fn print_timer_command_help(role: Option<Role>, command: &str) {
    match command {
        "start" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: timer start <ISSUE> --phase NAME --agent-role executor|reviewer --attempt N [--run-id ID]\n\nThe orchestrator writes a local ledger row before any remote operation. --agent-role is executor or reviewer; --attempt is a positive integer."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "finish" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: timer finish <RUN_ID> --result DONE|PARTIAL|BLOCKED|FAILED\n\nThe orchestrator records exact elapsed seconds, then projects them to the configured provider (Redmine or GitLab). Retries on the same run id are safe; the marker-based reconciliation short-circuits before any duplicate Time Entry or spent-time POST. Redmine receives the rounded 0.01-hour summary with a stable run-marker comment; GitLab receives the exact elapsed seconds in human-format duration with the marker embedded in the spent-time summary."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        _ => print_timer_help(role),
    }
}

fn print_workflow_help(role: Option<Role>) {
    if role.is_none_or(|role| role == Role::Admin) {
        println!(
            "Workflow commands for {}:\n\n  bootstrap        Find or create and configure the Redmine project\n\nUse 'phasegent --help workflow bootstrap' for options.",
            role.map_or("all roles", Role::as_str)
        );
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

fn print_auth_help(role: Option<Role>) {
    println!(
        "Authentication for {}:\n\n  setup              Store a provider credential securely\n\nOptions for setup:\n  --provider NAME     forgejo, redmine, or gitlab\n  --stdin             Read the credential from stdin\n  --api-base <URL>    Store the provider API base\n  --repository <O/R>  Store the Forgejo repository\n  --project-id <ID>   Store the Redmine or GitLab project id\n  --close-status-id <ID> Store the Redmine closed status\n\nCredentials and persisted provider config are role-scoped.\n--role is a capability policy, not identity isolation; credentials must still be least-privilege.\nCredentials are never accepted as command-line arguments.",
        role.map_or("all roles", Role::as_str)
    );
}

fn print_config_help(role: Option<Role>) {
    println!(
        "Local configuration for {}:\n\n  show               Print a redacted snapshot of the local SQLite database\n  import-env         Persist current PHASEGENT_* environment variables\n  provider get       Print the persisted machine-wide default provider (null when unset)\n  provider set NAME  Validate and persist the machine-wide default provider (forgejo, redmine, gitlab)\n  provider clear     Remove the persisted machine-wide default provider\n\nUse 'phasegent --help config <subcommand>' for options.\n`config show` and `config provider *` do not require --role because the global default and the global settings are machine-wide; `config import-env` does because most settings are role-scoped.",
        role.map_or("all roles", Role::as_str)
    );
}

fn print_config_command_help(role: Option<Role>, command: &str) {
    match command {
        "show" => {
            println!(
                "Usage: phasegent [config show]\n\nPrints a compact JSON snapshot of the local SQLite database:\n  database_path              absolute path to the SQLite file\n  roles                      array with one entry per role (admin, orchestrator, executor, reviewer)\n  global_settings            array of PHASEGENT_REDMINE_GIT_MIRROR_API_KEY, PHASEGENT_REDMINE_REPOSITORY_URL, and PHASEGENT_DEFAULT_PROVIDER\n  global_default_provider    machine-wide default provider literal (forgejo, redmine, or gitlab); null when unset\n\nCredential rows report presence and length only; the bearer key for the git mirror plugin is also reported as presence/length, and the repository URL override is sanitised so embedded userinfo, password, query, and fragment are stripped before the snapshot is rendered. The machine-wide default provider is a non-secret literal and is rendered both as a top-level field and inside `global_settings` so the snapshot stays self-contained.\n\nWith --role ROLE the snapshot is the same JSON with the roles array restricted to that single role."
            );
        }
        "import-env" => {
            let role_text = role.map_or("ROLE", Role::as_str);
            println!(
                "Usage: phasegent --role {role_text} config import-env\n\nPersists every PHASEGENT_* environment variable that is currently set in the process environment for the role selected by --role.\n\nRole-scoped variables:\n  PHASEGENT_PROVIDER\n  PHASEGENT_API_BASE\n  PHASEGENT_REPOSITORY\n  PHASEGENT_REDMINE_API_BASE\n  PHASEGENT_REDMINE_PROJECT_ID\n  PHASEGENT_REDMINE_CLOSE_STATUS_ID\n  PHASEGENT_PROJECT_ID                 (generic Redmine alias)\n  PHASEGENT_CLOSE_STATUS_ID            (generic Redmine alias)\n\nGlobal settings:\n  PHASEGENT_REDMINE_GIT_MIRROR_API_KEY\n  PHASEGENT_REDMINE_REPOSITORY_URL\n  PHASEGENT_DEFAULT_PROVIDER           (validated through ProviderKind; rejects unknown literals)\n\nThe command returns counts and a per-name report; secret values are never echoed. Environment variables are not modified by the command. Ordinary provider commands do not implicitly persist environment variables; persistence happens only through this explicit invocation."
            );
        }
        _ => print_config_help(role),
    }
}

fn print_config_provider_help() {
    println!(
        "Machine-wide default provider:\n\n  get               Print the persisted PHASEGENT_DEFAULT_PROVIDER (null when unset)\n  set NAME          Validate and persist the default (forgejo, redmine, or gitlab)\n  clear             Remove the persisted default so the resolver falls back to the role-scoped provider\n\n`config provider` subcommands do not require --role because the default is global. The resolver precedence is: explicit --provider > PHASEGENT_PROVIDER > PHASEGENT_DEFAULT_PROVIDER (env) > persisted PHASEGENT_DEFAULT_PROVIDER (SQLite) > role-scoped role_config.provider > forgejo fallback."
    );
}

fn print_config_provider_command_help(command: &str) {
    match command {
        "get" => {
            println!(
                "Usage: phasegent config provider get\n\nPrints a JSON object with the persisted PHASEGENT_DEFAULT_PROVIDER literal (`forgejo`, `redmine`, or `gitlab`) or `null` when the default has never been set. The output never echoes any secret value."
            );
        }
        "set" => {
            println!(
                "Usage: phasegent config provider set <forgejo|redmine|gitlab>\n\nValidates NAME through ProviderKind::from_str and persists the result in the global_setting table. Unknown literals return a structured config error before any write happens."
            );
        }
        "clear" => {
            println!(
                "Usage: phasegent config provider clear\n\nRemoves the PHASEGENT_DEFAULT_PROVIDER row from SQLite so the resolver falls back to the role-scoped provider. Returns {{\"cleared\": true}} when a row existed or {{\"cleared\": false}} when the default was already absent."
            );
        }
        _ => print_config_provider_help(),
    }
}

fn print_not_supported_help(operation: &str) {
    println!("No command available for Redmine: {operation} is Forgejo-only.");
}

fn print_issue_command_help(role: Option<Role>, command: &str) {
    let (capability, text) = match command {
        "get" => (Capability::IssueRead, "Usage: issue get <NUMBER>"),
        "search" => (
            Capability::IssueSearch,
            "Usage: issue search [--query TEXT] [--state open|closed|all]\n\nValues beginning with `-` must use the inline form: --query=TEXT or --state=STATE.",
        ),
        "create" => (
            Capability::IssueCreate,
            "Usage: issue create --title TEXT [--body TEXT] [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100]\n\n--tracker accepts a validated tracker name (Bug, Feature) or numeric id and is Redmine-only (GitLab maps it to a `type::bug` / `type::feature` label). Planning flags set native Redmine fields; --fixed-version resolves by exact version name or numeric id within the configured project. All Redmine planning flags are Redmine-only except --estimated-hours, which GitLab forwards through the native time_estimate endpoint. Forgejo rejects every planning flag.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --title=TEXT or --body=TEXT.",
        ),
        "update-body" => (
            Capability::IssueUpdateBody,
            "Usage: issue update-body <NUMBER> --body TEXT [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100]\n\n--tracker re-targets the issue's tracker in the same update (Redmine native; GitLab maps to a type::* label). Planning flags update native Redmine fields in the same PUT; --fixed-version resolves by exact version name or numeric id within the configured project. --estimated-hours is also accepted for GitLab (time_estimate); every other planning flag is Redmine-only. Forgejo rejects every planning flag.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --body=TEXT.",
        ),
        "close" => (Capability::IssueClose, "Usage: issue close <NUMBER>"),
        "bind" => (
            Capability::IssueRead,
            "Usage: issue bind <ID> [--replace]\n\nStores `branch.<name>.redmine-issue-id = <ID>` in the local Git config for the current named branch. Detached HEAD is rejected. A different existing binding is rejected unless --replace is given; re-binding the same issue is a no-op.",
        ),
        "unbind" => (
            Capability::IssueRead,
            "Usage: issue unbind\n\nRemoves the current branch's Redmine issue binding from the local Git config. Absence is a no-op.",
        ),
        "status" => (
            Capability::IssueRead,
            "Usage: issue status\n\nPrints the current branch and its bound Redmine issue, if any. Detached HEAD is an error.",
        ),
        _ => {
            print_issue_help(role);
            return;
        }
    };
    if role.is_none_or(|role| role.allows(capability)) {
        println!("{text}\n\n{}", capability.description());
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

fn print_comment_command_help(role: Option<Role>, command: &str) {
    let (capability, text) = match command {
        "create" => (
            Capability::CommentCreate,
            "Usage: comment create <ISSUE> --body TEXT --marker MARKER [--authorized]\n\nValues beginning with `-` must use the inline form: --body=TEXT or --marker=MARKER.",
        ),
        "get" => (
            Capability::CommentRead,
            "Usage: comment get <ISSUE> <COMMENT_ID>",
        ),
        "find-marker" => (
            Capability::CommentFindMarker,
            "Usage: comment find-marker <ISSUE> --marker MARKER\n\nMarker beginning with `-` must use the inline form: --marker=MARKER.",
        ),
        _ => {
            print_comment_help(role);
            return;
        }
    };
    if role.is_none_or(|role| role.allows(capability)) {
        println!("{text}\n\n{}", capability.description());
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

fn print_project_command_help(role: Option<Role>, command: &str) {
    let (capability, text) = match command {
        "list" => (Capability::ProjectRead, "Usage: project list"),
        "create" => (
            Capability::ProjectCreate,
            "Usage: project create --name NAME --identifier IDENTIFIER --confirm [--description TEXT]",
        ),
        _ => {
            print_project_help(role);
            return;
        }
    };
    if role.is_none_or(|role| role.allows(capability)) {
        println!("{text}\n\n{}", capability.description());
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

fn print_status_command_help(role: Option<Role>, command: &str) {
    if command != "list" && command != "set" {
        print_status_help(role);
        return;
    }
    let capability = Capability::IssueStatusRead;
    if command == "set" {
        if role.is_none_or(|role| role == Role::Orchestrator) {
            println!(
                "Usage: status set <NUMBER> --status NAME_OR_ID\n\nUpdates a Redmine issue status by validated numeric id or exact name and prints the updated issue. Orchestrator-only; Redmine-only.\n\nValues beginning with `-` must use the inline form: --status=VALUE."
            );
        } else {
            println!(
                "No command available for {}.",
                role.map_or("this role", Role::as_str)
            );
        }
        return;
    }
    if role.is_none_or(|role| role.allows(capability)) {
        println!("Usage: status list\n\n{}", capability.description());
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

fn print_workflow_command_help(role: Option<Role>, command: &str) {
    if command != "bootstrap" || !role.is_none_or(|role| role == Role::Admin) {
        print_workflow_help(role);
        return;
    }
    println!(
        "Usage: workflow bootstrap [--repository OWNER/REPOSITORY] [--close-status-id ID | --close-status-name NAME]\n\nFinds the exact Redmine project identifier derived from the repository, creates a missing private project automatically when missing, selects a closed issue status, then reconciles direct project memberships for the existing orchestrator (Maintainer), executor (Developer), and reviewer (Reporter) users. Each agent identity is resolved through that role's Redmine API key via `/users/current.json`; the admin API key performs project lookup/creation and the membership writes. The workflow is reported ready only when every direct membership is added, updated, or already present. Missing or ambiguous users or roles fail with an actionable error before any partial identity mapping is persisted."
    );
}

fn print_hooks_help() {
    println!(
        "Managed Git hook commands:\n\n  install          Install/update the managed prepare-commit-msg and commit-msg hooks\n\nUse 'phasegent --help hooks install' for options."
    );
}

fn print_hooks_command_help(command: &str) {
    if command != "install" {
        print_hooks_help();
        return;
    }
    println!(
        "Usage: hooks install\n\nInstalls or updates the managed prepare-commit-msg and commit-msg hooks in the current checkout's Git hooks directory. Existing unrelated hooks are preserved: they are moved to .git/hooks/phasegent-original/<hook-name> and chained so the original runs first. Managed hooks call `phasegent hooks run ...` locally and need no credentials; issue references come from the current branch's local Git config binding (`issue bind`)."
    );
}

pub(crate) fn required_role(role: Option<Role>) -> Role {
    role.expect("operation parsing requires a role")
}

pub(crate) fn permission_error(role: Role, capability: Capability) -> i32 {
    structured_error(
        serde_json::json!({
            "kind":"permission",
            "role":role.as_str(),
            "operation":capability.operation(),
            "message":format!("role '{}' is not allowed to perform {}", role, capability.operation())
        }),
        3,
    )
}

pub(crate) fn print_result<T: Serialize>(result: Result<T, ForgejoError>) -> i32 {
    match result {
        Ok(value) => print_json(&value),
        Err(error) => provider_error(error),
    }
}

fn print_json<T: Serialize>(value: &T) -> i32 {
    match serde_json::to_string(value) {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(error) => structured_error(
            serde_json::json!({"kind":"encode", "message":error.to_string()}),
            1,
        ),
    }
}

pub(crate) fn provider_error(error: ForgejoError) -> i32 {
    structured_error(error.json(), 1)
}

fn usage_error(message: &str) -> i32 {
    structured_error(serde_json::json!({"kind":"argument", "message":message}), 2)
}

fn structured_error(error: serde_json::Value, code: i32) -> i32 {
    eprintln!("{}", serde_json::json!({"error":error}));
    code
}
