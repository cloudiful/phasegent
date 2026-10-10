//! Remote bootstrap orchestration for the Redmine workflow.
//!
//! This root owns the public entry points ([`bootstrap`],
//! [`ensure_issue_workflow`]), the shared result types, and the per-session
//! bootstrap cache. The heavy lifting lives in cohesive children:
//!
//! - [`provision`]: role enumeration, membership defaults, distinct-user gate
//! - [`provision_user`]: one role's identity/key provisioning flow
//! - [`repository`]: repository, mirror URL, and bootstrap config resolution
//! - [`run`]: the resolved bootstrap sequence itself

mod provision;
mod provision_user;
mod repository;
mod run;

use crate::lifecycle;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::providers::redmine::model::{
    RedmineBootstrap, RedmineGitMirrorOutcome, RedmineUserMembershipOutcome,
};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use repository::{repository_was_explicit, resolve_bootstrap_config, resolve_repository};

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
        memberships_ready(&self.user_memberships)
    }
}

/// True when no membership warned, i.e. every one is added/updated/existing.
/// Shared by the bootstrap persistence gate and [`BootstrapResult::ready`] so
/// the two can never disagree on what "ready" means.
pub(super) fn memberships_ready(outcomes: &[RedmineUserMembershipOutcome]) -> bool {
    outcomes.iter().all(|outcome| outcome.status != "warning")
}

pub(crate) fn bootstrap(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    close_status_id: Option<&str>,
    close_status_name: Option<&str>,
) -> Result<BootstrapResult, PhasegentError> {
    let repository = resolve_repository(repository)?;
    let explicit_repository = repository_was_explicit(repository.as_str());
    let config = resolve_bootstrap_config(role, api_base, close_status_id)?;
    let close_status_id = close_status_name
        .is_none()
        .then(|| config.close_status_id.map(|value| value.to_string()))
        .flatten();
    run::bootstrap_resolved(
        run::WorkflowRoles::new(role, role),
        repository,
        explicit_repository,
        config,
        close_status_id.as_deref(),
        close_status_name,
    )
    .map(run::attach_local_hooks)
}

pub(crate) fn ensure_issue_workflow(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<WorkflowState, PhasegentError> {
    let repository = resolve_repository(repository)?;
    let explicit_repository = repository_was_explicit(repository.as_str());
    let config = resolve_bootstrap_config(Role::Admin, api_base, close_status_id)?;
    let key = format!("{}\0{}\0{}", role.as_str(), config.api_base, repository);
    let completed = completed_bootstraps();
    let mut completed = completed
        .lock()
        .map_err(|_| PhasegentError::config("workflow bootstrap state lock is poisoned"))?;
    if let Some(state) = completed.get(&key) {
        return Ok(state.clone());
    }

    let close_status_id = config.close_status_id.map(|value| value.to_string());
    let result = run::bootstrap_resolved(
        run::WorkflowRoles::new(Role::Admin, role),
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
        return Err(PhasegentError::config(detail));
    }
    let state = result.state();
    completed.insert(key, state.clone());
    Ok(state)
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
