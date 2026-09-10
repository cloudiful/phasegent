use crate::auth;
use crate::branch_context;
use crate::lifecycle;
use crate::policy::Role;
use crate::providers::api::ForgejoError;
use crate::providers::redmine;
use crate::providers::redmine::model::{
    DEFAULT_REDMINE_ROLE_EXECUTOR, DEFAULT_REDMINE_ROLE_ORCHESTRATOR,
    DEFAULT_REDMINE_ROLE_REVIEWER, DEFAULT_REDMINE_ROLE_TESTER, RedmineBootstrap,
    RedmineCurrentUser, RedmineGitMirrorOutcome, RedmineUserMembershipOutcome, provisioned_roles,
    provisioning_metadata,
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
    // Admin-only provisioning: the administrator credential is sufficient
    // for the entire bootstrap. Project lookup/creation, service-user
    // lookup/creation, API-key retrieval, and membership writes all use
    // the admin provider. Role-scoped keys are written for downstream
    // providers but never read for identity, and a missing admin key
    // never falls back to another role key.
    if roles.provider != Role::Admin {
        return Err(ForgejoError::config(
            "workflow bootstrap requires the admin Redmine API key; missing admin credential cannot fall back to another role key",
        ));
    }
    let admin = RedmineProvider::for_role(roles.provider, config.clone())?;
    let bootstrap =
        admin.bootstrap_project(&repository, &identifier, close_status_id, close_status_name)?;
    let provisioned = provision_agent_users(&admin)?;
    let orchestrator_user = find_provisioned(&provisioned, Role::Orchestrator)?;
    let executor_user = find_provisioned(&provisioned, Role::Executor)?;
    let reviewer_user = find_provisioned(&provisioned, Role::Reviewer)?;
    let tester_user = find_provisioned(&provisioned, Role::Tester)?;

    if orchestrator_user.id == executor_user.id
        || orchestrator_user.id == reviewer_user.id
        || orchestrator_user.id == tester_user.id
        || executor_user.id == reviewer_user.id
        || executor_user.id == tester_user.id
        || reviewer_user.id == tester_user.id
    {
        return Err(ForgejoError::config(format!(
            "Redmine role-scoped API keys must identify distinct users; got orchestrator={}, executor={}, reviewer={}, tester={}",
            describe_user(orchestrator_user),
            describe_user(executor_user),
            describe_user(reviewer_user),
            describe_user(tester_user)
        )));
    }

    let orchestrator = admin.ensure_user_membership(
        bootstrap.project.id,
        orchestrator_user,
        DEFAULT_REDMINE_ROLE_ORCHESTRATOR,
    )?;
    let executor = admin.ensure_user_membership(
        bootstrap.project.id,
        executor_user,
        DEFAULT_REDMINE_ROLE_EXECUTOR,
    )?;
    let reviewer = admin.ensure_user_membership(
        bootstrap.project.id,
        reviewer_user,
        DEFAULT_REDMINE_ROLE_REVIEWER,
    )?;
    let tester = admin.ensure_user_membership(
        bootstrap.project.id,
        tester_user,
        DEFAULT_REDMINE_ROLE_TESTER,
    )?;

    let all_memberships_ok = orchestrator.status != "warning"
        && executor.status != "warning"
        && reviewer.status != "warning"
        && tester.status != "warning";
    if all_memberships_ok {
        let storage = crate::infra::storage::Storage::open().map_err(ForgejoError::config)?;
        for (role, _) in &provisioned {
            auth::persist_redmine_bootstrap(
                *role,
                Some(config.api_base.clone()),
                bootstrap.project.id,
                bootstrap.close_status.id,
                &storage,
            )
            .map_err(ForgejoError::config)?;
        }
        if !provisioned.iter().any(|(role, _)| *role == roles.persist) {
            auth::persist_redmine_bootstrap(
                roles.persist,
                Some(config.api_base.clone()),
                bootstrap.project.id,
                bootstrap.close_status.id,
                &storage,
            )
            .map_err(ForgejoError::config)?;
        }
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

    let user_memberships = vec![orchestrator, executor, reviewer, tester];
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

/// Provision every built-in agent role through the administrator API.
///
/// Order follows [`provisioned_roles`] so membership reconciliation stays
/// deterministic. Each role reuses its persisted identity when both the
/// `role_redmine_user` row and the `role_credential` row are present;
/// otherwise the deterministic login is looked up before creating so
/// reruns and legacy databases never create duplicates.
fn provision_agent_users(
    admin: &RedmineProvider,
) -> Result<Vec<(Role, RedmineCurrentUser)>, ForgejoError> {
    let storage = crate::infra::storage::Storage::open().map_err(ForgejoError::config)?;
    let mut provisioned = Vec::with_capacity(4);
    for role in provisioned_roles() {
        let metadata = provisioning_metadata(role).ok_or_else(|| {
            ForgejoError::config(format!(
                "no provisioning metadata for role {}",
                role.as_str()
            ))
        })?;
        let user = provision_single_role(admin, &storage, role, &metadata)?;
        provisioned.push((role, user));
    }
    Ok(provisioned)
}

fn find_provisioned(
    provisioned: &[(Role, RedmineCurrentUser)],
    role: Role,
) -> Result<&RedmineCurrentUser, ForgejoError> {
    provisioned
        .iter()
        .find(|(candidate, _)| *candidate == role)
        .map(|(_, user)| user)
        .ok_or_else(|| {
            ForgejoError::config(format!(
                "provisioned user for role {} is missing",
                role.as_str()
            ))
        })
}

/// Provision one agent role: reuse the persisted identity when present,
/// otherwise look up the deterministic login and create only when absent.
///
/// The administrator provider performs every HTTP call (lookup, create,
/// API-key read). No role-scoped key is read for identity, so a missing
/// admin key fails here without falling back to another role key. The
/// retrieved API key and the user identity are persisted before
/// returning; generated passwords never leave the server because
/// creation uses Redmine password generation.
fn provision_single_role(
    admin: &RedmineProvider,
    storage: &crate::infra::storage::Storage,
    role: Role,
    metadata: &crate::providers::redmine::model::RoleProvisioningMetadata,
) -> Result<RedmineCurrentUser, ForgejoError> {
    use crate::infra::storage::PROVIDER_REDMINE;

    let persisted_user = auth::load_redmine_user(role, storage).map_err(ForgejoError::config)?;
    let persisted_key = storage
        .load_credential(role, PROVIDER_REDMINE)
        .map_err(ForgejoError::config)?;
    if let (Some((user_id, login)), Some(api_key)) = (persisted_user, persisted_key)
        && user_id > 0
        && !login.trim().is_empty()
        && !api_key.trim().is_empty()
    {
        return Ok(RedmineCurrentUser {
            id: user_id,
            login,
            firstname: String::new(),
            lastname: String::new(),
            mail: String::new(),
        });
    }

    if let Some(existing) = admin.find_user_by_login(metadata.login).map_err(|error| {
        ForgejoError::config(format!(
            "could not lookup the {} user '{}': {}",
            role.as_str(),
            metadata.login,
            describe(&error)
        ))
    })? {
        let api_key = admin.get_user_api_key(existing.id).map_err(|error| {
            ForgejoError::config(format!(
                "could not retrieve the {} user API key: {}",
                role.as_str(),
                describe(&error)
            ))
        })?;
        auth::save_redmine_user(role, existing.id, &existing.login, storage)
            .map_err(ForgejoError::config)?;
        storage
            .save_credential(role, PROVIDER_REDMINE, &api_key)
            .map_err(ForgejoError::config)?;
        return Ok(RedmineCurrentUser {
            id: existing.id,
            login: existing.login,
            firstname: existing.firstname,
            lastname: existing.lastname,
            mail: existing.mail,
        });
    }

    let created = match admin.create_service_user(
        metadata.login,
        metadata.firstname,
        metadata.lastname,
        metadata.mail,
    ) {
        Ok(user) => user,
        Err(error) if is_duplicate_login(&error) => {
            let recovered = admin.find_user_by_login(metadata.login).map_err(|inner| {
                ForgejoError::config(format!(
                    "could not lookup the {} user '{}' after duplicate: {}",
                    role.as_str(),
                    metadata.login,
                    describe(&inner)
                ))
            })?;
            let existing = recovered.ok_or_else(|| {
                ForgejoError::config(format!(
                    "Redmine user '{}' already exists but lookup found nothing",
                    metadata.login
                ))
            })?;
            let api_key = admin.get_user_api_key(existing.id).map_err(|inner| {
                ForgejoError::config(format!(
                    "could not retrieve the {} user API key: {}",
                    role.as_str(),
                    describe(&inner)
                ))
            })?;
            auth::save_redmine_user(role, existing.id, &existing.login, storage)
                .map_err(ForgejoError::config)?;
            storage
                .save_credential(role, PROVIDER_REDMINE, &api_key)
                .map_err(ForgejoError::config)?;
            return Ok(RedmineCurrentUser {
                id: existing.id,
                login: existing.login,
                firstname: existing.firstname,
                lastname: existing.lastname,
                mail: existing.mail,
            });
        }
        Err(error) => {
            return Err(ForgejoError::config(format!(
                "could not create the {} user '{}': {}",
                role.as_str(),
                metadata.login,
                describe(&error)
            )));
        }
    };
    let api_key = admin.get_user_api_key(created.id).map_err(|error| {
        ForgejoError::config(format!(
            "could not retrieve the {} user API key: {}",
            role.as_str(),
            describe(&error)
        ))
    })?;
    auth::save_redmine_user(role, created.id, &created.login, storage)
        .map_err(ForgejoError::config)?;
    storage
        .save_credential(role, PROVIDER_REDMINE, &api_key)
        .map_err(ForgejoError::config)?;
    Ok(RedmineCurrentUser {
        id: created.id,
        login: created.login,
        firstname: created.firstname,
        lastname: created.lastname,
        mail: created.mail,
    })
}

fn is_duplicate_login(error: &ForgejoError) -> bool {
    match error {
        ForgejoError::Http {
            status: 422,
            message,
            ..
        } => {
            let lower = message.to_ascii_lowercase();
            lower.contains("already been taken")
                || lower.contains("has already")
                || lower.contains("duplicate")
        }
        _ => false,
    }
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

#[cfg(test)]
pub(crate) fn clear_completed_bootstraps_for_tests() {
    if let Ok(mut map) = completed_bootstraps().lock() {
        map.clear();
    }
}
