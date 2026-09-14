//! Redmine project resolution for repository-aware workflows.
//!
//! - Explicit `--project-id` is the highest-priority override and never
//!   triggers discovery.
//! - Otherwise, when the provider is Redmine, the current Git origin is
//!   resolved and `discover_matching_projects` is consulted.
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
use crate::providers::api::{ForgejoError, IssueSummary};
use crate::providers::{IssueProvider, ProviderKind, RedmineConfig, RedmineProvider};

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

/// Expected project for single-number Redmine operations (issue 394).
///
/// Resolve-only wrapper around [`resolve_redmine_project`]: explicit
/// `--project-id` wins, otherwise origin + `discover_single`. `Ok(None)`
/// (no match or explicit-repository mismatch) becomes an actionable
/// config error; multiple matches and discovery errors propagate. Never
/// calls `ensure_issue_workflow` so single-number paths never
/// auto-bootstrap.
pub(crate) fn resolve_expected_redmine_project(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    explicit_project_id: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<String, ForgejoError> {
    match resolve_redmine_project(
        role,
        api_base,
        repository,
        explicit_project_id,
        close_status_id,
    ) {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err(ForgejoError::config(
            "cannot determine the expected Redmine project for this single-number operation: \
             no Redmine project matches the current Git checkout; pass --project-id to select \
             explicitly (single-number operations never auto-create a project)",
        )),
        Err(error) => Err(error),
    }
}

/// Shared project comparison for the single-number scope guard
/// (issue 394). Numeric expectations compare the project id;
/// identifier/name expectations compare `identifier`/`name`.
fn guard_project_parts(
    actual_id: u64,
    actual_name: &str,
    actual_identifier: Option<&str>,
    expected_project: &str,
    number: u64,
) -> Result<(), ForgejoError> {
    let expected = expected_project.trim();
    if expected.is_empty() {
        return Err(ForgejoError::config(
            "expected Redmine project must be non-empty; pass --project-id to select explicitly",
        ));
    }
    if let Ok(expected_id) = expected.parse::<u64>() {
        if actual_id == expected_id {
            return Ok(());
        }
        return Err(ForgejoError::config(format!(
            "Redmine issue {number} belongs to project {actual_id} ('{actual_name}'), not expected project \
             '{expected}' for the current Git checkout; pass --project-id '{actual_id}' to allow \
             cross-project access or correct the number",
        )));
    }
    let matches_identifier = actual_identifier.is_some_and(|identifier| identifier == expected);
    if matches_identifier || actual_name == expected {
        return Ok(());
    }
    Err(ForgejoError::config(format!(
        "Redmine issue {number} belongs to project {actual_id} ('{actual_name}'), not expected project \
         '{expected}' for the current Git checkout; pass --project-id to select explicitly",
    )))
}

/// Guard a fetched [`IssueSummary`] against the expected project
/// (issue 394). This is the single shared guard every single-number CLI
/// entry uses (via [`enforce_redmine_single_number_scope`] for reads and
/// [`verify_redmine_scope_before_write`] for pre-write checks). Missing
/// `project` is unverifiable and rejected; non-Redmine summaries always
/// carry `None` and must be skipped by the caller via the enforce helper
/// (which no-ops for non-Redmine).
pub(crate) fn guard_issue_summary_project(
    summary: &IssueSummary,
    expected_project: &str,
    number: u64,
) -> Result<(), ForgejoError> {
    let Some(project) = summary.project.as_ref() else {
        let expected = expected_project.trim();
        return Err(ForgejoError::config(format!(
            "Redmine issue {number} response omits project; cannot verify it belongs to expected \
             project '{expected}'; pass --project-id to select explicitly"
        )));
    };
    guard_project_parts(
        project.id,
        &project.name,
        project.identifier.as_deref(),
        expected_project,
        number,
    )
}

/// Resolve the expected project and guard one fetched summary (issue 394).
/// No-op for non-Redmine providers so every single-number CLI arm can call
/// this one helper unconditionally. Single-number paths never bootstrap:
/// an unresolvable checkout fails with the actionable
/// `resolve_expected` error.
#[allow(clippy::too_many_arguments)]
pub(crate) fn enforce_redmine_single_number_scope(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    explicit_project_id: Option<&str>,
    close_status_id: Option<&str>,
    provider_kind: ProviderKind,
    summary: &IssueSummary,
    number: u64,
) -> Result<(), ForgejoError> {
    if provider_kind != ProviderKind::Redmine {
        return Ok(());
    }
    let expected = resolve_expected_redmine_project(
        role,
        api_base,
        repository,
        explicit_project_id,
        close_status_id,
    )?;
    guard_issue_summary_project(summary, &expected, number)
}

/// Pre-write scope check for single-number Redmine operations (issue 394 P3).
///
/// Resolves the expected project, fetches the issue with a read-only GET,
/// then guards the fetched summary. Fails before any PUT so a cross-project
/// number never writes. No-op for non-Redmine providers (no GET issued).
/// Reuses [`resolve_expected_redmine_project`] plus
/// [`guard_issue_summary_project`]; no new discovery chains.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_redmine_scope_before_write<P>(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    explicit_project_id: Option<&str>,
    close_status_id: Option<&str>,
    provider_kind: ProviderKind,
    provider: &P,
    number: u64,
) -> Result<(), ForgejoError>
where
    P: IssueProvider<Error = ForgejoError>,
{
    if provider_kind != ProviderKind::Redmine {
        return Ok(());
    }
    let expected = resolve_expected_redmine_project(
        role,
        api_base,
        repository,
        explicit_project_id,
        close_status_id,
    )?;
    let summary = provider.get_issue(number)?;
    guard_issue_summary_project(&summary, &expected, number)
}

/// Shared Redmine discovery + bootstrap for search/create paths
/// (issue 394 P2 dedup). Calls [`resolve_redmine_project`]; a discovered
/// id is used directly with the caller's close id, while `NoMatch`
/// falls back to [`crate::workflow::ensure_issue_workflow`]. Discovery
/// errors propagate. Single-number paths must NOT use this helper;
/// they use [`resolve_expected_redmine_project`] (resolve-only, never
/// bootstraps).
pub(crate) fn resolve_redmine_project_for_search_or_create(
    role: Role,
    api_base: Option<&str>,
    repository: Option<&str>,
    explicit_project_id: Option<&str>,
    close_status_id: Option<&str>,
) -> Result<(Option<String>, Option<String>), ForgejoError> {
    match resolve_redmine_project(
        role,
        api_base,
        repository,
        explicit_project_id,
        close_status_id,
    )? {
        Some(discovered) => Ok((Some(discovered), close_status_id.map(str::to_owned))),
        None => {
            let state = crate::workflow::ensure_issue_workflow(
                role,
                api_base,
                repository,
                close_status_id,
            )?;
            Ok((
                Some(state.project_id),
                Some(state.close_status_id.to_string()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{guard_issue_summary_project, resolve_expected_redmine_project};
    use crate::policy::Role;
    use crate::providers::api::{IssueProjectRef, IssueSummary};
    use crate::providers::redmine::model::RedmineIssue;

    fn summary_with_project(
        id: u64,
        project_id: u64,
        name: &str,
        identifier: Option<&str>,
    ) -> IssueSummary {
        IssueSummary {
            id,
            number: id,
            title: format!("Issue {id}"),
            body: String::new(),
            state: "open".to_owned(),
            html_url: None,
            project: Some(IssueProjectRef {
                id: project_id,
                name: name.to_owned(),
                identifier: identifier.map(str::to_owned),
            }),
        }
    }

    fn summary_without_project(id: u64) -> IssueSummary {
        IssueSummary {
            id,
            number: id,
            title: format!("Issue {id}"),
            body: String::new(),
            state: "open".to_owned(),
            html_url: None,
            project: None,
        }
    }

    #[test]
    fn guard_accepts_numeric_match() {
        let summary = summary_with_project(17, 42, "Phasegent", Some("phasegent"));
        assert!(guard_issue_summary_project(&summary, "42", 17).is_ok());
    }

    #[test]
    fn guard_rejects_numeric_mismatch_with_hint() {
        let summary = summary_with_project(17, 99, "Other", Some("other"));
        let error = guard_issue_summary_project(&summary, "42", 17).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("17"), "missing number: {message}");
        assert!(message.contains("99"), "missing actual: {message}");
        assert!(message.contains("--project-id"), "missing hint: {message}");
    }

    #[test]
    fn guard_rejects_missing_project_as_unverifiable() {
        let summary = summary_without_project(17);
        let error = guard_issue_summary_project(&summary, "42", 17).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("omits project"), "got: {message}");
    }

    #[test]
    fn guard_accepts_identifier_and_name_matches() {
        let summary = summary_with_project(17, 42, "Phasegent", Some("phasegent"));
        assert!(guard_issue_summary_project(&summary, "phasegent", 17).is_ok());
        assert!(guard_issue_summary_project(&summary, "Phasegent", 17).is_ok());
    }

    #[test]
    fn guard_rejects_identifier_mismatch() {
        let summary = summary_with_project(17, 42, "Phasegent", Some("phasegent"));
        assert!(guard_issue_summary_project(&summary, "other", 17).is_err());
    }

    #[test]
    fn resolve_expected_prefers_explicit_without_network() {
        let resolved =
            resolve_expected_redmine_project(Role::Executor, None, None, Some("42"), None).unwrap();
        assert_eq!(resolved, "42");
    }

    #[test]
    fn project_deserializes_and_defaults_to_none() {
        let with: RedmineIssue = serde_json::from_value(
            serde_json::json!({"id": 17, "project": {"id": 42, "name": "Phasegent"}}),
        )
        .unwrap();
        let summary = with.into_summary("https://example/issues/17".to_owned());
        let project = summary.project.as_ref().expect("project passthrough");
        assert_eq!(project.id, 42);
        assert_eq!(project.name, "Phasegent");
        let without: RedmineIssue = serde_json::from_value(serde_json::json!({"id": 18})).unwrap();
        let summary = without.into_summary("https://example/issues/18".to_owned());
        assert!(summary.project.is_none());
    }
}
