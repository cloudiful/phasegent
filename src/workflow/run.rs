//! The resolved bootstrap sequence: project discovery, service-user
//! provisioning, membership reconciliation, mirror registration, and the
//! optional local hook install.

use super::BootstrapResult;
use super::provision::{ProvisionedRole, membership_role_name, provision_agent_users};
use super::repository::{resolve_mirror_url, split_repository};
use crate::auth;
use crate::git_runner;
use crate::lifecycle;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::providers::redmine;
use crate::providers::redmine::model::{RedmineBootstrap, RedmineUserMembershipOutcome};
use crate::providers::{RedmineConfig, RedmineProvider};
use crate::remote;

/// Provider role used for every remote call, and the role whose settings the
/// bootstrap persists when it is not itself a provisioned service role.
#[derive(Clone, Copy)]
pub(super) struct WorkflowRoles {
    provider: Role,
    persist: Role,
}

impl WorkflowRoles {
    pub(super) fn new(provider: Role, persist: Role) -> Self {
        Self { provider, persist }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn bootstrap_resolved(
    roles: WorkflowRoles,
    repository: String,
    explicit_repository: bool,
    config: RedmineConfig,
    close_status_id: Option<&str>,
    close_status_name: Option<&str>,
) -> Result<BootstrapResult, PhasegentError> {
    let identifier = remote::redmine_identifier(&repository).map_err(PhasegentError::config)?;
    // Admin-only provisioning: the administrator credential is sufficient
    // for the entire bootstrap. Project lookup/creation, service-user
    // lookup/creation, API-key retrieval, and membership writes all use
    // the admin provider. Role-scoped keys are written for downstream
    // providers but never read for identity, and a missing admin key
    // never falls back to another role key.
    if roles.provider != Role::Admin {
        return Err(PhasegentError::config(
            "workflow bootstrap requires the admin Redmine API key; missing admin credential cannot fall back to another role key",
        ));
    }
    let admin = RedmineProvider::for_role(roles.provider, config.clone())?;
    let bootstrap =
        admin.bootstrap_project(&repository, &identifier, close_status_id, close_status_name)?;
    let provisioned = provision_agent_users(&admin)?;
    let user_memberships = reconcile_memberships(&admin, bootstrap.project.id, &provisioned)?;
    if super::memberships_ready(&user_memberships) {
        persist_role_settings(&provisioned, roles.persist, &config, &bootstrap)?;
    }

    // Register the current repository's Git URL with the `redmine_git_mirror`
    // plugin. Registration is idempotent: a GET against the deterministic
    // `mirror_<project_id>_<owner>_<repo>` identifier short-circuits the
    // POST when the mirror already exists. Mirror HTTP errors and a
    // `failed` status fail bootstrap clearly so operators see the cause.
    let (owner, repo_name) = split_repository(&repository).map_err(PhasegentError::config)?;
    let mirror_url =
        resolve_mirror_url(&repository, explicit_repository).map_err(PhasegentError::config)?;
    let git_mirror = redmine::register_git_mirror(
        config.api_base.as_str(),
        bootstrap.project.id,
        owner.as_str(),
        repo_name.as_str(),
        &mirror_url,
    )?;

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

/// Reconcile the direct project membership of every provisioned role, in
/// provision order, using each role's shared default Redmine role name.
///
/// Every membership is attempted even when an earlier one warns so the
/// operator sees the full outcome set; the caller decides whether the
/// warnings block persistence.
fn reconcile_memberships(
    admin: &RedmineProvider,
    project_id: u64,
    provisioned: &[ProvisionedRole],
) -> Result<Vec<RedmineUserMembershipOutcome>, PhasegentError> {
    let mut memberships = Vec::with_capacity(provisioned.len());
    for entry in provisioned {
        let role_name = membership_role_name(entry.role)?;
        memberships.push(admin.ensure_user_membership(project_id, &entry.user, role_name)?);
    }
    Ok(memberships)
}

/// Persist the resolved project/close status and provider preference for
/// every provisioned role, plus the requesting role when it is not one of
/// them. Only reached when every membership is actionable.
fn persist_role_settings(
    provisioned: &[ProvisionedRole],
    persist: Role,
    config: &RedmineConfig,
    bootstrap: &RedmineBootstrap,
) -> Result<(), PhasegentError> {
    let storage = crate::infra::storage::Storage::open().map_err(PhasegentError::config)?;
    for entry in provisioned {
        auth::persist_redmine_bootstrap(
            entry.role,
            Some(config.api_base.clone()),
            bootstrap.project.id,
            bootstrap.close_status.id,
            &storage,
        )
        .map_err(PhasegentError::config)?;
    }
    if !provisioned.iter().any(|entry| entry.role == persist) {
        auth::persist_redmine_bootstrap(
            persist,
            Some(config.api_base.clone()),
            bootstrap.project.id,
            bootstrap.close_status.id,
            &storage,
        )
        .map_err(PhasegentError::config)?;
    }
    Ok(())
}

/// Attempts managed hook installation for the current checkout and stores the
/// structured outcome on the bootstrap result. Never fails: a local hook
/// problem becomes a warning inside the outcome.
pub(super) fn attach_local_hooks(mut result: BootstrapResult) -> BootstrapResult {
    let working_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            result.hooks = Some(lifecycle::HookAutoInstall::Failed {
                reason: format!("cannot resolve working directory: {error}"),
            });
            return result;
        }
    };
    let runner = git_runner::ProcessGitRunner::new();
    result.hooks = Some(lifecycle::auto_install_hooks(
        &runner,
        &working_dir,
        &result.repository,
    ));
    result
}
