//! Redmine project resolution for repository-aware workflows.
//!
//! Implements Phase 3's repository-aware Redmine project selection:
//!
//! - Explicit `--project-id` is the highest-priority override and never
//!   triggers discovery.
//! - Otherwise, when the provider is Redmine, the current Git origin is
//!   resolved and Phase 2's `discover_matching_projects` is consulted.
//!   Exactly one `remote_url` match supplies the project id for the
//!   current invocation; multiple matches fail with a bounded actionable
//!   error listing candidate ids/names; discovery HTTP/auth/decode
//!   errors are propagated, not treated as `NoMatch`.
//! - An explicit `--repository` that does not identify the current origin
//!   never uses the origin's match; the caller falls back to the
//!   existing explicit-repository/bootstrap path.
//! - The helper constructs a `RedmineProvider` with
//!   `RedmineConfig::resolve` without a project id solely for read-only
//!   discovery and never persists the discovered id.

use crate::policy::Role;
use crate::providers::api::ForgejoError;
use crate::providers::{RedmineConfig, RedmineProvider};

/// Resolve a Redmine project id for the current invocation.
///
/// Returns:
/// - `Ok(Some(id))` when an explicit `--project-id` is present or when
///   discovery finds exactly one matching project. The id is not persisted.
/// - `Ok(None)` when discovery finds no match (caller decides whether to
///   bootstrap or return an actionable error).
/// - `Err` when discovery finds multiple matches (bounded listing) or when
///   any discovery HTTP/auth/decode error occurs. Those errors are never
///   swallowed as `NoMatch`.
///
/// Discovery is not performed when `explicit_project_id` is `Some` and
/// non-empty. An explicit `--repository` that does not equal the current
/// Git origin's `OWNER/REPOSITORY` also skips discovery and returns
/// `Ok(None)` so the caller preserves the existing explicit-repository
/// bootstrap behavior.
pub(crate) fn resolve_redmine_project(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    explicit_project_id: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<Option<String>, ForgejoError> {
    if let Some(id) = explicit_project_id {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_owned()));
        }
    }

    // Need the current Git origin to perform discovery. Origin resolution
    // is the credential-free identity used for mirror matching.
    let origin = match crate::remote::resolve_origin() {
        Ok(origin) => origin,
        Err(error) => {
            // When the caller supplied an explicit repository, skip
            // discovery and let the existing bootstrap path handle the
            // explicit repository (which will require
            // PHASEGENT_REDMINE_REPOSITORY_URL or an explicit project id).
            if repository.is_some() {
                return Ok(None);
            }
            return Err(ForgejoError::config(error));
        }
    };

    resolve_with_origin(
        role,
        api_base,
        repository,
        explicit_project_id,
        close_status_id,
        &origin,
    )
}

/// Testable variant that uses an already-resolved origin. The `repository`
/// explicit override check and the explicit-project-id short-circuit are
/// preserved so tests can exercise mismatch handling without touching the
/// real Git checkout.
pub(crate) fn resolve_with_origin(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    explicit_project_id: Option<&str>,
    close_status_id: Option<&str>,
    origin: &crate::remote::RemoteRepository,
) -> Result<Option<String>, ForgejoError> {
    if let Some(id) = explicit_project_id {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_owned()));
        }
    }

    if let Some(explicit_repo) = repository {
        let trimmed = explicit_repo.trim();
        if !trimmed.is_empty() && trimmed != origin.repository {
            return Ok(None);
        }
    }

    let config = RedmineConfig::resolve(role, api_base, None, close_status_id)?;
    let provider = RedmineProvider::for_role(role, config)?;

    match provider.discover_matching_projects(origin) {
        Ok(discovery) => match discovery {
            crate::providers::redmine::RedmineDiscovery::NoMatch => Ok(None),
            crate::providers::redmine::RedmineDiscovery::Single(project) => {
                Ok(Some(project.id.to_string()))
            }
            crate::providers::redmine::RedmineDiscovery::Multiple(projects) => {
                let mut message = format!(
                    "multiple Redmine projects match the current Git origin '{}': ",
                    origin.repository
                );
                let limit = 10;
                let candidates: Vec<String> = projects
                    .iter()
                    .take(limit)
                    .map(|project| format!("{} '{}'", project.id, project.name))
                    .collect();
                message.push_str(&candidates.join(", "));
                if projects.len() > limit {
                    message.push_str(&format!(" (and {} more)", projects.len() - limit));
                }
                message.push_str("; pass --project-id to select one");
                Err(ForgejoError::config(message))
            }
        },
        Err(error) => Err(error),
    }
}
