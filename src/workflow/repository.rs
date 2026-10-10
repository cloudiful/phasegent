//! Repository, mirror URL, and bootstrap config resolution.
//!
//! Kept apart from the bootstrap sequence so the local Git/origin and
//! storage lookups stay testable on their own and never pull the
//! provisioning flow into scope.

use crate::auth;
use crate::policy::Role;
use crate::providers::RedmineConfig;
use crate::remote;

/// Resolve the bootstrap repository from the explicit argument or the local
/// Git origin.
pub(super) fn resolve_repository(
    repository: Option<&str>,
) -> Result<String, crate::providers::api::PhasegentError> {
    match repository {
        Some(repository) => remote::validate_repository(repository)
            .map_err(crate::providers::api::PhasegentError::config),
        None => remote::resolve_origin()
            .map(|remote| remote.repository)
            .map_err(crate::providers::api::PhasegentError::config),
    }
}

/// Track whether the caller supplied `--repository` so the mirror URL
/// resolution can require an explicit env override when the bootstrap
/// repository does not match the local Git origin.
pub(super) fn repository_was_explicit(repository: &str) -> bool {
    if let Ok(origin) = remote::resolve_origin() {
        return origin.repository != repository;
    }
    // Outside a git checkout the bootstrap would have failed already; treat
    // any reachable repository argument as explicit so we surface the
    // missing-env-url error rather than silently use a stale URL.
    true
}

/// Split `OWNER/REPOSITORY` into the deterministic mirror identifier parts.
pub(super) fn split_repository(repository: &str) -> Result<(String, String), String> {
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
pub(super) fn resolve_mirror_url(
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

/// Resolve the admin-side Redmine config for a bootstrap run.
///
/// The bootstrap always runs as the administrator and resolves the project
/// itself, so the role-scoped project id is dropped: discovery never trusts
/// a stale stored identifier.
pub(super) fn resolve_bootstrap_config(
    role: Role,
    api_base: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<RedmineConfig, crate::providers::api::PhasegentError> {
    let mut config = RedmineConfig::resolve(role, api_base, None, close_status_id)?;
    config.project_id = None;
    Ok(config)
}
