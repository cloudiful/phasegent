use crate::auth;
use crate::command::{self, Command, IssueCommand};
use crate::infra::storage::Storage;
use crate::policy::{Capability, Role};
use crate::providers::config::resolve_kind;
use crate::providers::forgejo::{ForgejoConfig, ForgejoError};
use crate::providers::{GitlabConfig, ProviderDispatcher, ProviderKind, RedmineConfig};
use serde::Serialize;

mod branch;
mod comment;
mod help;
mod hooks;
mod issue;
mod issue_index;
mod project;
mod project_resolution;
mod relation;
mod repo;
mod status;
mod version;
mod workflow;

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

fn execute(invocation: crate::command::Invocation) -> i32 {
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
        Command::ConfigSet {
            setting,
            value,
            stdin,
        } => {
            let result = open_storage().and_then(|storage| {
                crate::config::set_json(
                    invocation.role,
                    &setting,
                    value.as_deref(),
                    stdin,
                    &storage,
                )
            });
            match result {
                Ok(outcome) => print_json(&outcome),
                Err(message) => {
                    structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
                }
            }
        }
        Command::ConfigClear { setting } => {
            let result = open_storage()
                .and_then(|storage| crate::config::clear_json(invocation.role, &setting, &storage));
            match result {
                Ok(outcome) => print_json(&outcome),
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
        ) => branch::execute_branch_context(command),
        Command::Issue(command) => issue::execute_issue(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Comment(command) => comment::execute_comment(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Project(command) => project::execute_project(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Status(command) => status::execute_status(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::VersionCommand(command) => version::execute_version(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Workflow(command) => workflow::execute_workflow(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.close_status_id.as_deref(),
            invocation.close_status_name.as_deref(),
            command,
        ),
        Command::Repo(command) => repo::execute_repo_or_gitlab(
            invocation.role,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Relation(command) => relation::execute_relation(
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
                crate::time_tracking::execute(
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
            | command::TimerCommand::Recover { .. } => crate::time_tracking::execute_recovery(
                invocation.role,
                invocation.provider,
                invocation.api_base.as_deref(),
                invocation.project_id.as_deref(),
                invocation.close_status_id.as_deref(),
                command,
            )
            .map_or_else(crate::cli::provider_error, |output| print_json(&output)),
        },
        Command::Hooks(command) => hooks::execute_hooks(command),
    }
}

/// Emits bounded lifecycle warnings on stderr so stdout JSON stays compatible
/// with the plain provider output shape.
pub(crate) fn report_local_warnings(operation: &str, warnings: Option<String>) {
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

pub(crate) fn provider_for(
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

pub(crate) fn print_json<T: Serialize>(value: &T) -> i32 {
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

pub(crate) fn usage_error(message: &str) -> i32 {
    structured_error(serde_json::json!({"kind":"argument", "message":message}), 2)
}

pub(crate) fn structured_error(error: serde_json::Value, code: i32) -> i32 {
    eprintln!("{}", serde_json::json!({"error":error}));
    code
}
