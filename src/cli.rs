use crate::auth;
use crate::command::{self, Command, IssueCommand};
use crate::infra::storage::Storage;
use crate::output::context::OutputMode;
use crate::policy::{CallerContext, Capability, Role};
use crate::providers::api::PhasegentError;
use crate::providers::config::resolve_kind;
use crate::providers::{ProviderDispatcher, ProviderKind, RedmineConfig};
use serde::Serialize;

pub(crate) mod branch;
mod comment;
pub(crate) mod doctor;
mod help;
mod hooks;
pub(crate) mod issue;
pub(crate) mod issue_branches;
pub(crate) mod issue_closure;
mod notify;
pub(crate) mod plugin;
mod project;
mod project_resolution;
pub(crate) mod provider_context;
mod record;
mod relation;
mod scope_guard;
mod skill;
mod status;
pub(crate) mod task_route;
mod version;
mod workflow;
mod workflow_all_projects;
pub(crate) mod worktree;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(args: impl IntoIterator<Item = String>) -> i32 {
    let args = args.into_iter().collect::<Vec<_>>();
    // The presentation scope is installed before the parse, so a parser or
    // permission failure already renders in the requested format.
    let options = command::prescan_presentation(&args);
    let _scope = crate::output::context::install(&options, command::caller_from_env().ok());
    match command::parse_outcome(&args) {
        Ok(command::ParseOutcome::Invocation(invocation)) => execute(*invocation),
        Ok(command::ParseOutcome::Permission { role, operation }) => {
            permission_denial(role, operation)
        }
        Err(message) => usage_error(&message),
    }
}

/// Role-injectable entry point for in-process tests. Mirrors [`run`] but
/// takes an already verified agent role directly, so a test never has to
/// mutate the process-global environment or run a host broker.
#[cfg(test)]
pub(crate) fn run_with_role(args: impl IntoIterator<Item = String>, role_env: Option<&str>) -> i32 {
    let args = args.into_iter().collect::<Vec<_>>();
    let options = command::prescan_presentation(&args);
    let caller = command::caller_from(role_env, false).ok();
    let _scope = crate::output::context::install(&options, caller);
    match command::parse_outcome_with_role_env(&args, role_env) {
        Ok(command::ParseOutcome::Invocation(invocation)) => execute(*invocation),
        Ok(command::ParseOutcome::Permission { role, operation }) => {
            permission_denial(role, operation)
        }
        Err(message) => usage_error(&message),
    }
}

/// Fully injectable entry point for deterministic caller tests: `role_env`
/// models an already verified agent role, `session_present` models the
/// managed session marker. Parallel tests use this instead of mutating
/// process env.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn run_with_caller(
    args: impl IntoIterator<Item = String>,
    role_env: Option<&str>,
    session_present: bool,
) -> i32 {
    let args = args.into_iter().collect::<Vec<_>>();
    let options = command::prescan_presentation(&args);
    let caller = command::caller_from(role_env, session_present).ok();
    let _scope = crate::output::context::install(&options, caller);
    match command::parse_outcome_with_caller_env(&args, role_env, session_present) {
        Ok(command::ParseOutcome::Invocation(invocation)) => execute(*invocation),
        Ok(command::ParseOutcome::Permission { role, operation }) => {
            permission_denial(role, operation)
        }
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
    // The parsed command refines the scoped presentation: the authoritative
    // options, the human view, and the command label.
    crate::output::context::apply(&invocation.output, invocation.caller);
    crate::output::context::update(|context| {
        context.view = crate::output::context::view_for(&invocation.command);
        context.label = crate::output::context::label_for(&invocation.command).to_owned();
    });
    // The progress surface starts only for the command groups that may take
    // a while. The credential, the stdin, the help, and the version surfaces
    // never animate, and the guard clears the ticker when the command ends.
    let context = crate::output::context::current_or_default();
    let _progress = if shows_progress(&invocation.command) {
        Some(crate::output::progress::ProgressGuard::start(
            &context,
            &context.label,
        ))
    } else {
        None
    };
    let provider_invocation = provider_context::ProviderContext::from_invocation(&invocation);
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
            target_role,
        } => {
            // Provisioning is a human-operator surface. A verified agent or a
            // managed session without a role is rejected before any secret or
            // storage access, even when it tries to choose a target row.
            let role =
                match operator_supply_scope(invocation.caller, target_role, "admin auth setup") {
                    Ok(target) => target.unwrap_or(Role::Admin),
                    Err(code) => return code,
                };
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
        Command::Doctor => doctor::execute_doctor(),
        Command::ConfigShow => {
            // The CLI re-uses `invocation.role` so a verified agent role
            // gets a single-role view, and `phasegent config show` (no
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
            target_role,
        } => {
            let write_role =
                match operator_supply_scope(invocation.caller, target_role, "admin config set") {
                    Ok(role) => role,
                    Err(code) => return code,
                };
            let result = open_storage().and_then(|storage| {
                crate::config::set_json(write_role, &setting, value.as_deref(), stdin, &storage)
            });
            match result {
                Ok(outcome) => print_json(&outcome),
                Err(message) => {
                    structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
                }
            }
        }
        Command::ConfigClear {
            setting,
            target_role,
        } => {
            let write_role =
                match operator_supply_scope(invocation.caller, target_role, "admin config clear") {
                    Ok(role) => role,
                    Err(code) => return code,
                };
            let result = open_storage()
                .and_then(|storage| crate::config::clear_json(write_role, &setting, &storage));
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
        // Local branch context commands resolve their durable link
        // scope locally (no provider construction, no network); a flow
        // with no resolvable scope fails closed instead of writing.
        // `issue branches N` is the read-only reverse lookup across all
        // scopes in this repository (no provider, no writes).
        Command::Issue(IssueCommand::Branches { number, page }) => {
            issue_branches::execute_branches_page(number, page)
        }
        Command::Issue(
            command @ (IssueCommand::Bind { .. }
            | IssueCommand::Unbind { .. }
            | IssueCommand::StatusBranch),
        ) => branch::execute_branch_context_scoped(invocation.role, &provider_invocation, command),
        Command::Issue(command) => issue::execute_issue_with_caller(
            invocation.caller,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Comment(command) => comment::execute_comment_with_caller(
            invocation.caller,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Record(command) => record::execute_record_with_caller(
            invocation.caller,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Project(command) => project::execute_project_with_caller(
            invocation.caller,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Status(command) => status::execute_status_with_caller(
            invocation.caller,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::VersionCommand(command) => version::execute_version_with_caller(
            invocation.caller,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Workflow(command) => workflow::execute_workflow_with_caller(
            invocation.caller,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.close_status_id.as_deref(),
            invocation.close_status_name.as_deref(),
            invocation.project_id.as_deref(),
            command,
        ),
        Command::Relation(command) => relation::execute_relation_with_caller(
            invocation.caller,
            invocation.provider,
            invocation.api_base.as_deref(),
            invocation.repository.as_deref(),
            invocation.project_id.as_deref(),
            invocation.close_status_id.as_deref(),
            command,
        ),
        Command::Timer(command) => match &command {
            command::TimerCommand::Start { .. } | command::TimerCommand::Finish { .. } => {
                crate::time_tracking::execute_with_caller(
                    invocation.caller,
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
            | command::TimerCommand::Recover { .. } => {
                crate::time_tracking::execute_recovery_with_caller(
                    invocation.caller,
                    invocation.provider,
                    invocation.api_base.as_deref(),
                    invocation.project_id.as_deref(),
                    invocation.close_status_id.as_deref(),
                    command,
                )
                .map_or_else(crate::cli::provider_error, |output| print_json(&output))
            }
        },
        Command::Hooks(command) => hooks::execute_hooks(command),
        Command::Plugin(command) => plugin::execute_plugin(command),
        Command::Skill(command) => skill::execute_skill(command),
        Command::Notify(command) => notify::execute_notify_with_caller(invocation.caller, command),
        Command::Worktree(command) => {
            worktree::execute_worktree_scoped(invocation.caller, &provider_invocation, command)
        }
    }
}

/// The command groups that show a progress surface: the provider requests and
/// the local worktree and sync operations. The help, the version, the
/// credential and stdin surfaces, the read-only local checks, and the
/// notification surface never animate.
fn shows_progress(command: &Command) -> bool {
    use crate::command::WorktreeCommand;
    match command {
        Command::Issue(_)
        | Command::Comment(_)
        | Command::Record(_)
        | Command::Project(_)
        | Command::Status(_)
        | Command::VersionCommand(_)
        | Command::Workflow(_)
        | Command::Relation(_)
        | Command::Timer(_) => true,
        Command::Worktree(
            WorktreeCommand::Acquire { .. }
            | WorktreeCommand::List { .. }
            | WorktreeCommand::Prune { .. },
        ) => true,
        Command::Help(_)
        | Command::Version
        | Command::AuthSetup { .. }
        | Command::ConfigShow
        | Command::ConfigSet { .. }
        | Command::ConfigClear { .. }
        | Command::ConfigProviderGet
        | Command::ConfigProviderSet { .. }
        | Command::ConfigProviderClear
        | Command::Hooks(_)
        | Command::Plugin(_)
        | Command::Skill(_)
        | Command::Notify(_)
        | Command::Doctor
        | Command::Worktree(_) => false,
    }
}

/// Emits bounded lifecycle warnings on stderr so stdout JSON stays compatible
/// with the plain provider output shape.
pub(crate) fn report_local_warnings(operation: &str, warnings: Option<String>) {
    let Some(reason) = warnings else {
        return;
    };
    let context = crate::output::context::current_or_default();
    // The ticker is cleared before the warning so the two never overlap.
    crate::output::progress::clear();
    match context.effective_mode() {
        OutputMode::Json => eprintln!(
            "{}",
            serde_json::json!({
                "warning": {
                    "operation": operation,
                    "message": reason,
                }
            })
        ),
        OutputMode::Text => eprintln!(
            "{}",
            crate::output::render::render_diagnostic(
                "warning",
                &serde_json::json!({"operation": operation, "message": reason}),
                context.stderr_color,
            )
        ),
    }
}

pub(crate) fn provider_for(
    role: Role,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<ProviderDispatcher, PhasegentError> {
    // An unknown provider name errors here, before any provider is built.
    match resolve_kind(role, provider_kind)? {
        ProviderKind::Redmine => {
            let config = RedmineConfig::resolve(role, api_base, project_id, close_status_id)?;
            ProviderDispatcher::redmine(role, config)
        }
        ProviderKind::Forgejo => {
            if project_id.is_some() || close_status_id.is_some() {
                return Err(PhasegentError::config(
                    "Forgejo does not support --project-id or --close-status-id",
                ));
            }
            let config =
                crate::providers::forgejo::ForgejoConfig::resolve(role, api_base, repository)?;
            ProviderDispatcher::forgejo(role, config)
        }
        ProviderKind::Gitlab => {
            if project_id.is_some() || close_status_id.is_some() {
                return Err(PhasegentError::config(
                    "GitLab does not support --project-id or --close-status-id",
                ));
            }
            let config =
                crate::providers::gitlab::GitlabConfig::resolve(role, api_base, repository)?;
            ProviderDispatcher::gitlab(role, config)
        }
        // Local needs no credentials or remote config: open the
        // independent SQLite store directly. Auth stays passwordless.
        ProviderKind::Local => {
            let _ = repository;
            crate::providers::local::LocalProvider::open().map(ProviderDispatcher::local)
        }
    }
}

#[allow(dead_code)]
pub(crate) fn required_role(role: Option<Role>) -> Role {
    role.expect("operation parsing requires a role")
}

/// Human-only provisioning scope. `--target-role` selects the storage row,
/// never the caller identity; a verified agent or a managed session without a
/// role is refused here, before any secret or storage access. `None` keeps the
/// existing default (Admin for credential writes, the config layer's central
/// resolver for settings).
fn operator_supply_scope(
    caller: CallerContext,
    target_role: Option<Role>,
    operation: &str,
) -> Result<Option<Role>, i32> {
    match caller {
        CallerContext::Operator => Ok(target_role),
        CallerContext::Agent(role) => Err(permission_denial(role, operation)),
        CallerContext::ManagedWithoutRole => Err(managed_denial(operation)),
    }
}

/// Fail-closed denial for a managed session without a verified role
/// attempting a business/permissioned command. Fires before any credential,
/// provider, or side effect; utility commands never reach here.
///
/// The wording keeps the established "managed session requires a role"
/// prefix; the hint names the verified host context instead of the legacy
/// self-selected role variable, which no longer grants an identity.
pub(crate) fn managed_denial(operation: &str) -> i32 {
    structured_error(
        serde_json::json!({
            "kind":"permission",
            "operation":operation,
            "message":managed_denial_message(operation)
        }),
        3,
    )
}

/// Fail-closed denial for a managed session without a role at the config
/// layer (which returns strings, not exit codes).
pub(crate) fn managed_denial_message(operation: &str) -> String {
    let mut message = format!(
        "managed session requires a role for {operation}; the host did not verify this session"
    );
    if let Some(reason) = crate::host_context::last_denial() {
        message.push_str(&format!(" ({reason})"));
    }
    message
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

/// Structured denial for a command the parser registry rejected. Emits the
/// same `permission` envelope and exit code as the execution-layer gate so the
/// failure point moves earlier without changing the observable contract.
fn permission_denial(role: Role, operation: &str) -> i32 {
    structured_error(
        serde_json::json!({
            "kind":"permission",
            "role":role.as_str(),
            "operation":operation,
            "message":command::permission_message(role, operation),
        }),
        3,
    )
}

pub(crate) fn print_result<T: Serialize>(result: Result<T, PhasegentError>) -> i32 {
    match result {
        Ok(value) => print_json(&value),
        Err(error) => provider_error(error),
    }
}

/// The central success emitter. The call signature stays the compatibility
/// adapter every command uses; the scoped context decides whether the result
/// reaches stdout as the stable JSON document or as readable text.
pub(crate) fn print_json<T: Serialize>(value: &T) -> i32 {
    let context = crate::output::context::current_or_default();
    // The ticker is cleared before the result so the document never overlaps
    // the progress line.
    crate::output::progress::clear();
    match context.effective_mode() {
        OutputMode::Json => match serde_json::to_string(value) {
            Ok(value) => {
                println!("{value}");
                0
            }
            Err(error) => encode_error(&error),
        },
        OutputMode::Text => match serde_json::to_value(value) {
            Ok(value) => {
                println!("{}", crate::output::render::render_value(&value, &context));
                0
            }
            Err(error) => encode_error(&error),
        },
    }
}

fn encode_error(error: &serde_json::Error) -> i32 {
    structured_error(
        serde_json::json!({"kind":"encode", "message":error.to_string()}),
        1,
    )
}

pub(crate) fn provider_error(error: PhasegentError) -> i32 {
    structured_error(error.json(), 1)
}

pub(crate) fn usage_error(message: &str) -> i32 {
    structured_error(serde_json::json!({"kind":"argument", "message":message}), 2)
}

/// The central failure emitter. The JSON envelope is unchanged; the text
/// rendering is a readable diagnostic label with the same operation and
/// message.
pub(crate) fn structured_error(error: serde_json::Value, code: i32) -> i32 {
    let context = crate::output::context::current_or_default();
    // The ticker is cleared before the diagnostic so the two never overlap.
    crate::output::progress::clear();
    match context.effective_mode() {
        OutputMode::Json => eprintln!("{}", serde_json::json!({"error":error})),
        OutputMode::Text => eprintln!(
            "{}",
            crate::output::render::render_diagnostic("error", &error, context.stderr_color)
        ),
    }
    code
}

#[cfg(test)]
mod provider_selection_tests {
    use super::*;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    #[test]
    fn an_environment_forgejo_default_is_admitted_through_the_common_path() {
        let _lock = lock_workflow_tests();
        let directory = crate::test_scratch::root().join(format!(
            "phasegent-provider-selection-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let database = directory.join(crate::infra::storage::DB_FILENAME);
        let _database = EnvGuard::set("PHASEGENT_DB_PATH", database.to_string_lossy().as_ref());
        let _env = EnvGuard::set("PHASEGENT_PROVIDER", "forgejo");
        let error = match provider_for(
            Role::Orchestrator,
            None,
            Some("http://127.0.0.1:1"),
            Some("owner/repo"),
            None,
            None,
        ) {
            Ok(_) => panic!("an empty store has no role credential to build the provider"),
            Err(error) => error,
        };
        let message = error.json()["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            !message.contains("requires explicit --provider forgejo"),
            "a configured Forgejo default is admitted: {message}"
        );
        assert!(
            message.contains("credential") || message.contains("not configured"),
            "the empty store fails on provenance, not admission: {message}"
        );
    }
}
