use crate::auth;
use crate::branch_context;
use crate::lifecycle;
use crate::policy::Role;
use crate::providers::api::ForgejoError;
use crate::providers::redmine;
use crate::providers::redmine::model::{
    DEFAULT_REDMINE_ROLE_EXECUTOR, DEFAULT_REDMINE_ROLE_ORCHESTRATOR,
    DEFAULT_REDMINE_ROLE_REVIEWER, DEFAULT_REDMINE_ROLE_TESTER, RedmineBootstrap,
    RedmineGitMirrorOutcome, RedmineUserMembershipOutcome,
};
use crate::providers::{RedmineConfig, RedmineProvider};
use crate::remote;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkflowState {
    pub(crate) project_id: String,
    pub(crate) close_status_id: u64,
}

#[derive(Debug)]
pub(crate) struct BootstrapResult {
    pub(crate) repository: String,
    pub(crate) identifier: String,
    pub(crate) bootstrap: RedmineBootstrap,
    pub(crate) user_memberships: Vec<RedmineUserMembershipOutcome>,
    pub(crate) git_mirror: Option<RedmineGitMirrorOutcome>,
    /// Local managed-hook installation attempted only when the current
    /// checkout's origin matches the bootstrap repository; never fails the
    /// remote bootstrap.
    pub(crate) hooks: Option<lifecycle::HookAutoInstall>,
}

#[derive(Clone, Copy)]
struct WorkflowRoles {
    provider: Role,
    persist: Role,
}

impl BootstrapResult {
    pub(crate) fn state(&self) -> WorkflowState {
        WorkflowState {
            project_id: self.bootstrap.project.id.to_string(),
            close_status_id: self.bootstrap.close_status.id,
        }
    }

    /// True when every required user membership ended up in an actionable
    /// (added/updated/existing) state. A bootstrap with any warning
    /// membership is not considered ready.
    pub(crate) fn ready(&self) -> bool {
        self.user_memberships
            .iter()
            .all(|outcome| outcome.status != "warning")
    }
}

pub(crate) fn bootstrap(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    close_status_id: Option<&str>,
    close_status_name: Option<&str>,
) -> Result<BootstrapResult, ForgejoError> {
    let repository = resolve_repository(repository)?;
    let explicit_repository = repository_was_explicit(repository.as_str());
    let config = resolve_bootstrap_config(role, api_base, close_status_id)?;
    let close_status_id = close_status_name
        .is_none()
        .then(|| config.close_status_id.map(|value| value.to_string()))
        .flatten();
    bootstrap_resolved(
        WorkflowRoles {
            provider: role,
            persist: role,
        },
        repository,
        explicit_repository,
        config,
        close_status_id.as_deref(),
        close_status_name,
    )
    .map(attach_local_hooks)
}

pub(crate) fn ensure_issue_workflow(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<WorkflowState, ForgejoError> {
    let repository = resolve_repository(repository)?;
    let explicit_repository = repository_was_explicit(repository.as_str());
    let config = resolve_bootstrap_config(Role::Admin, api_base, close_status_id)?;
    let key = format!("{}\0{}\0{}", role.as_str(), config.api_base, repository);
    let completed = completed_bootstraps();
    let mut completed = completed
        .lock()
        .map_err(|_| ForgejoError::config("workflow bootstrap state lock is poisoned"))?;
    if let Some(state) = completed.get(&key) {
        return Ok(state.clone());
    }

    let close_status_id = config.close_status_id.map(|value| value.to_string());
    let result = bootstrap_resolved(
        WorkflowRoles {
            provider: Role::Admin,
            persist: role,
        },
        repository,
        explicit_repository,
        config,
        close_status_id.as_deref(),
        None,
    )?;
    if !result.ready() {
        let detail = result
            .user_memberships
            .iter()
            .find_map(|outcome| outcome.warning.clone())
            .unwrap_or_else(|| "Redmine direct user memberships could not be ensured".to_owned());
        return Err(ForgejoError::config(detail));
    }
    let state = result.state();
    completed.insert(key, state.clone());
    Ok(state)
}

#[allow(clippy::too_many_arguments)]
fn bootstrap_resolved(
    roles: WorkflowRoles,
    repository: String,
    explicit_repository: bool,
    config: RedmineConfig,
    close_status_id: Option<&str>,
    close_status_name: Option<&str>,
) -> Result<BootstrapResult, ForgejoError> {
    let identifier = remote::redmine_identifier(&repository).map_err(ForgejoError::config)?;
    // The admin credential performs project lookup/creation and the
    // membership writes. Agent identities are resolved separately with each
    // role-scoped key so that "no shared AI Agents group" still leaves a
    // concrete Redmine user for every role.
    let admin = RedmineProvider::for_role(roles.provider, config.clone())?;
    let bootstrap =
        admin.bootstrap_project(&repository, &identifier, close_status_id, close_status_name)?;
    let orchestrator_role = DEFAULT_REDMINE_ROLE_ORCHESTRATOR;
    let executor_role = DEFAULT_REDMINE_ROLE_EXECUTOR;
    let reviewer_role = DEFAULT_REDMINE_ROLE_REVIEWER;
    let tester_role = DEFAULT_REDMINE_ROLE_TESTER;

    let orchestrator_user = identify_agent_user(&config, Role::Orchestrator)?;
    let executor_user = identify_agent_user(&config, Role::Executor)?;
    let reviewer_user = identify_agent_user(&config, Role::Reviewer)?;
    let tester_user = tester_user_if_configured(&config)?;

    if orchestrator_user.id == executor_user.id
        || orchestrator_user.id == reviewer_user.id
        || executor_user.id == reviewer_user.id
    {
        return Err(ForgejoError::config(format!(
            "Redmine role-scoped API keys must identify distinct users; got orchestrator={}, executor={}, reviewer={}",
            describe_user(&orchestrator_user),
            describe_user(&executor_user),
            describe_user(&reviewer_user)
        )));
    }
    if let Some(ref tester) = tester_user {
        if tester.id == orchestrator_user.id
            || tester.id == executor_user.id
            || tester.id == reviewer_user.id
        {
            return Err(ForgejoError::config(format!(
                "Redmine role-scoped API keys must identify distinct users; got orchestrator={}, executor={}, reviewer={}, tester={}",
                describe_user(&orchestrator_user),
                describe_user(&executor_user),
                describe_user(&reviewer_user),
                describe_user(tester)
            )));
        }
    }

    let orchestrator = admin.ensure_user_membership(
        bootstrap.project.id,
        &orchestrator_user,
        orchestrator_role,
    )?;
    let executor =
        admin.ensure_user_membership(bootstrap.project.id, &executor_user, executor_role)?;
    let reviewer =
        admin.ensure_user_membership(bootstrap.project.id, &reviewer_user, reviewer_role)?;
    let tester_membership = match tester_user {
        Some(ref tester) => {
            Some(admin.ensure_user_membership(bootstrap.project.id, tester, tester_role)?)
        }
        None => None,
    };

    let all_memberships_ok = orchestrator.status != "warning"
        && executor.status != "warning"
        && reviewer.status != "warning"
        && tester_membership
            .as_ref()
            .is_none_or(|outcome| outcome.status != "warning");
    if all_memberships_ok {
        let storage = crate::infra::storage::Storage::open().map_err(ForgejoError::config)?;
        auth::persist_redmine_bootstrap(
            roles.persist,
            Some(config.api_base.clone()),
            bootstrap.project.id,
            bootstrap.close_status.id,
            &storage,
        )
        .map_err(ForgejoError::config)?;
    }

    // Register the current repository's Git URL with the `redmine_git_mirror`
    // plugin. Registration is idempotent: a GET against the deterministic
    // `mirror_<project_id>_<owner>_<repo>` identifier short-circuits the
    // POST when the mirror already exists. Mirror HTTP errors and a
    // `failed` status fail bootstrap clearly so operators see the cause.
    let (owner, repo_name) = split_repository(&repository).map_err(ForgejoError::config)?;
    let mirror_url =
        resolve_mirror_url(&repository, explicit_repository).map_err(ForgejoError::config)?;
    let git_mirror = redmine::register_git_mirror(
        config.api_base.as_str(),
        bootstrap.project.id,
        owner.as_str(),
        repo_name.as_str(),
        &mirror_url,
    )?;

    let mut user_memberships = vec![orchestrator, executor, reviewer];
    if let Some(tester) = tester_membership {
        user_memberships.push(tester);
    }
    Ok(BootstrapResult {
        repository,
        identifier,
        bootstrap,
        user_memberships,
        git_mirror: Some(git_mirror),
        // Set by the explicit bootstrap entry point; implicit workflow
        // bootstrapping (issue search/create) never touches local hooks.
        hooks: None,
    })
}

/// Attempts managed hook installation for the current checkout and stores the
/// structured outcome on the bootstrap result. Never fails: a local hook
/// problem becomes a warning inside the outcome.
fn attach_local_hooks(mut result: BootstrapResult) -> BootstrapResult {
    let working_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            result.hooks = Some(lifecycle::HookAutoInstall::Failed {
                reason: format!("cannot resolve working directory: {error}"),
            });
            return result;
        }
    };
    let runner = branch_context::ProcessGitRunner::new();
    result.hooks = Some(lifecycle::auto_install_hooks(
        &runner,
        &working_dir,
        &result.repository,
    ));
    result
}

fn identify_agent_user(
    config: &RedmineConfig,
    role: Role,
) -> Result<crate::providers::redmine::model::RedmineCurrentUser, ForgejoError> {
    let provider = RedmineProvider::for_role(role, config.clone())?;
    provider.current_user().map_err(|error| {
        ForgejoError::config(format!(
            "could not identify the {} user via {}: {}",
            role.as_str(),
            role.as_str(),
            describe(&error)
        ))
    })
}

fn tester_user_if_configured(
    config: &RedmineConfig,
) -> Result<Option<crate::providers::redmine::model::RedmineCurrentUser>, ForgejoError> {
    let storage = crate::infra::storage::Storage::open().map_err(ForgejoError::config)?;
    let has_tester = storage
        .load_credential(Role::Tester, crate::infra::storage::PROVIDER_REDMINE)
        .map_err(ForgejoError::config)?
        .is_some();
    if !has_tester {
        return Ok(None);
    }
    let user = identify_agent_user(config, Role::Tester)?;
    Ok(Some(user))
}

fn resolve_repository(repository: Option<&str>) -> Result<String, ForgejoError> {
    match repository {
        Some(repository) => remote::validate_repository(repository).map_err(ForgejoError::config),
        None => remote::resolve_origin()
            .map(|remote| remote.repository)
            .map_err(ForgejoError::config),
    }
}

/// Track whether the caller supplied `--repository` so the mirror URL
/// resolution can require an explicit env override when the bootstrap
/// repository does not match the local Git origin.
fn repository_was_explicit(repository: &str) -> bool {
    if let Ok(origin) = remote::resolve_origin() {
        return origin.repository != repository;
    }
    // Outside a git checkout the bootstrap would have failed already; treat
    // any reachable repository argument as explicit so we surface the
    // missing-env-url error rather than silently use a stale URL.
    true
}

fn split_repository(repository: &str) -> Result<(String, String), String> {
    let mut parts = repository.split('/');
    let owner = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "mirror identifier requires an owner".to_owned())?;
    let repo = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "mirror identifier requires a repository".to_owned())?;
    if parts.next().is_some() {
        return Err("mirror identifier requires OWNER/REPOSITORY form".to_owned());
    }
    Ok((owner.to_owned(), repo.to_owned()))
}

/// Pick the URL passed to the mirror plugin. `PHASEGENT_REDMINE_REPOSITORY_URL`
/// always wins; otherwise we use the credential-stripped origin URL. When the
/// bootstrap repository was supplied explicitly and does not match the
/// origin we refuse to silently send the wrong repository, requiring the env
/// override instead.
fn resolve_mirror_url(
    bootstrap_repository: &str,
    explicit_repository: bool,
) -> Result<String, String> {
    let storage = crate::infra::storage::Storage::open()?;
    let env_url = auth::redmine_repository_url_override(&storage)?;
    if let Some(env_url) = env_url {
        return Ok(env_url);
    }
    let origin = remote::resolve_origin()?;
    if explicit_repository && origin.repository != bootstrap_repository {
        return Err(format!(
            "--repository {bootstrap_repository} does not match the local git origin; \
             set PHASEGENT_REDMINE_REPOSITORY_URL to specify the mirror URL explicitly"
        ));
    }
    if origin.repository_url.trim().is_empty() {
        return Err("git origin resolved without a usable URL".to_owned());
    }
    Ok(origin.repository_url)
}

fn resolve_bootstrap_config(
    role: Role,
    api_base: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<RedmineConfig, ForgejoError> {
    let mut config = RedmineConfig::resolve(role, api_base, None, close_status_id)?;
    config.project_id = None;
    Ok(config)
}

fn describe(error: &ForgejoError) -> String {
    let json = error.json();
    json.get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| error.to_string())
}

fn describe_user(user: &crate::providers::redmine::model::RedmineCurrentUser) -> String {
    if !user.login.is_empty() {
        user.login.clone()
    } else {
        format!("#{}", user.id)
    }
}

fn completed_bootstraps() -> &'static Mutex<HashMap<String, WorkflowState>> {
    static COMPLETED: OnceLock<Mutex<HashMap<String, WorkflowState>>> = OnceLock::new();
    COMPLETED.get_or_init(|| Mutex::new(HashMap::new()))
}
