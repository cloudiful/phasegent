//! Local-only link scope resolution: provider literal plus project.
//!
//! No network, no provider discovery, and no provider-default fallback: a
//! scope that cannot be selected explicitly or read from stored config is
//! reported as unresolved so callers keep legacy behavior (or a structured
//! BLOCKED-style error) instead of assigning issues to an unrelated
//! provider/project. Role-less invocations resolve stored config as the
//! orchestrator for scope purposes only; permission passthrough is
//! unchanged and lives in the CLI layer.

//! P3 integration API.
#![allow(dead_code)]

use crate::policy::Role;
use crate::providers::ProviderKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkScope {
    /// Provider literal for [`crate::branch_links::IssueKey`]
    /// (`redmine`/`gitlab`/`local`).
    pub provider: String,
    /// Stable project identifier for that provider (`owner/repo`,
    /// Redmine project id, GitLab numeric id as string, or `default`).
    pub project: String,
}

/// Resolve the link scope from explicit CLI overrides plus stored
/// config, without any network access. Returns `Ok(None)` when no
/// provider can be selected without guessing; returns `Err` when a
/// selected provider lacks its required project (BLOCKED-style: the
/// caller must not assign the issue elsewhere).
pub fn resolve_link_scope(
    role: Option<Role>,
    explicit_provider: Option<ProviderKind>,
    // Retained for call-site compatibility; repository scoping lives
    // in the issue-branch origin check, not the link scope.
    _repository: Option<&str>,
    project_id: Option<&str>,
) -> Result<Option<LinkScope>, String> {
    let effective = role.unwrap_or(Role::Orchestrator);
    let literal = match select_provider_literal(effective, explicit_provider)? {
        Some(literal) => literal,
        None => return Ok(None),
    };
    match literal.as_str() {
        "redmine" => match project_id.map(str::trim).filter(|v| !v.is_empty()) {
            Some(project) => Ok(Some(LinkScope {
                provider: "redmine".to_owned(),
                project: project.to_owned(),
            })),
            None => Err(
                "cannot scope branch link: redmine links need an explicit --project-id; \
                refusing to assign the issue without a project"
                    .to_owned(),
            ),
        },
        "gitlab" => match project_id.map(str::trim).filter(|v| !v.is_empty()) {
            Some(raw) => match raw.parse::<u64>() {
                Ok(number) if number > 0 => Ok(Some(LinkScope {
                    provider: "gitlab".to_owned(),
                    project: number.to_string(),
                })),
                _ => Err(
                    "cannot scope branch link: gitlab links need an explicit numeric \
                    --project-id"
                        .to_owned(),
                ),
            },
            None => Err(
                "cannot scope branch link: gitlab links need an explicit numeric \
                --project-id; refusing to assign the issue without a project"
                    .to_owned(),
            ),
        },
        "local" => Ok(Some(LinkScope {
            provider: "local".to_owned(),
            project: "default".to_owned(),
        })),
        other => Err(format!(
            "cannot scope branch link: unknown provider '{other}'"
        )),
    }
}

/// Select the provider literal without a provider-default fallback:
/// explicit flag, then `PHASEGENT_PROVIDER` / `PHASEGENT_DEFAULT_PROVIDER`
/// env, then stored effective role config. `Ok(None)` means nothing
/// selected the provider, so the caller must not guess one.
fn select_provider_literal(
    role: Role,
    explicit: Option<ProviderKind>,
) -> Result<Option<String>, String> {
    if let Some(kind) = explicit {
        return Ok(Some(kind.as_str().to_owned()));
    }
    for name in ["PHASEGENT_PROVIDER", "PHASEGENT_DEFAULT_PROVIDER"] {
        if let Ok(raw) = std::env::var(name) {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let kind: ProviderKind = trimmed
                .parse()
                .map_err(|error: String| format!("cannot scope branch link: {error}"))?;
            return Ok(Some(kind.as_str().to_owned()));
        }
    }
    stored_provider(role)
}

fn stored_provider(role: Role) -> Result<Option<String>, String> {
    let storage = match crate::infra::storage::Storage::open() {
        Ok(storage) => storage,
        Err(_) => return Ok(None),
    };
    let config = crate::auth::load_config(role, &storage).map_err(|error| {
        format!("cannot scope branch link: could not read stored config: {error}")
    })?;
    let literal = config
        .and_then(|config| config.provider)
        .map(|provider| provider.trim().to_owned())
        .filter(|provider| !provider.is_empty());
    let Some(literal) = literal else {
        return Ok(None);
    };
    // A stale stored literal fails closed with the same actionable
    // guidance as the provider resolver instead of scoping the link
    // to an unsupported value. The row is left intact for explicit
    // clear/replace; link rows stay opaque strings.
    let kind: ProviderKind = literal
        .parse()
        .map_err(|error: String| format!("cannot scope branch link: {error}"))?;
    Ok(Some(kind.as_str().to_owned()))
}
