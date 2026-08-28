use crate::auth;
use crate::command::{
    self, Command, CommentCommand, HooksCommand, Invocation, IssueCommand, ProjectCommand,
    RelationCommand, StatusCommand, VersionCommand, WorkflowCommand,
};
use crate::infra::storage::Storage;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::{ForgejoConfig, ForgejoError};
use crate::providers::{
    GitlabConfig, IssueProvider, ProviderDispatcher, ProviderKind, RedmineConfig,
    RedmineMetadataProvider, RedmineProvider,
};
use crate::workflow;
use serde::Serialize;

mod help;

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
            help::print_help(invocation.role, invocation.provider, topic);
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
        Command::Timer(command) => match &command {
            command::TimerCommand::Start { .. } | command::TimerCommand::Finish { .. } => {
                crate::time_tracking_cli::execute(
                    invocation.role,
                    invocation.provider,
                    invocation.api_base.as_deref(),
                    invocation.project_id.as_deref(),
                    invocation.close_status_id.as_deref(),
                    command,
                )
                .map_or_else(crate::cli::provider_error, |output| print_json(&output))
            }
            command::TimerCommand::List { .. }
            | command::TimerCommand::Get { .. }
            | command::TimerCommand::Recover { .. } => crate::time_tracking_cli::execute_recovery(
                invocation.role,
                invocation.provider,
                invocation.api_base.as_deref(),
                invocation.project_id.as_deref(),
                invocation.close_status_id.as_deref(),
                command,
            )
            .map_or_else(crate::cli::provider_error, |output| print_json(&output)),
        },
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
        } => print_result(crate::providers::redmine::planning::update_body(
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
    if matches!(
        command,
        StatusCommand::Set { .. } | StatusCommand::Advance { .. }
    ) && role != Role::Orchestrator
    {
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
        // `next` and `advance` are Redmine-only capabilities: the
        // canonical policy is expressed with Redmine status names and
        // resolved against this installation's status ids.
        StatusCommand::Next { number } => match provider {
            ProviderDispatcher::Redmine(redmine) => print_result(redmine.status_next(number)),
            other => provider_error(ForgejoError::not_supported(
                other.kind().as_str(),
                "issue status next",
            )),
        },
        StatusCommand::Advance { number, status } => match provider {
            ProviderDispatcher::Redmine(redmine) => {
                print_result(redmine.advance_issue_status(number, &status))
            }
            other => provider_error(ForgejoError::not_supported(
                other.kind().as_str(),
                "issue status advance",
            )),
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
    match crate::providers::redmine::relations::execute(&provider, &command) {
        Ok(crate::providers::redmine::relations::RelationResult::List(relations)) => {
            print_json(&relations)
        }
        Ok(crate::providers::redmine::relations::RelationResult::Created(summary)) => {
            print_json(&summary)
        }
        Ok(crate::providers::redmine::relations::RelationResult::Deleted(relation_id)) => {
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
