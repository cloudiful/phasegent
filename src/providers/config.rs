use crate::auth;
use crate::ci_model::{
    CiInspectOutput, CiInspectRequest, CiJobLogsOutput, CiJobsOutput, CiRunSummary, CiRunsFilter,
    CiRunsOutput,
};
use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::api::{ForgejoError, RepoSummary};
use crate::providers::redmine::http::RedmineHttp;
use crate::remote;
use std::str::FromStr;
use url::Url;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderKind {
    #[default]
    Forgejo,
    Redmine,
    /// GitLab provider. Added in the Phase 1 foundation so the resolver,
    /// dispatcher, config snapshot, and env-import paths all recognise
    /// `gitlab` before the HTTP layer lands in subsequent phases.
    Gitlab,
}

impl ProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forgejo => "forgejo",
            Self::Redmine => "redmine",
            Self::Gitlab => "gitlab",
        }
    }
}

impl FromStr for ProviderKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "forgejo" => Ok(Self::Forgejo),
            "redmine" => Ok(Self::Redmine),
            "gitlab" => Ok(Self::Gitlab),
            _ => Err(format!(
                "invalid provider '{value}'; expected forgejo, redmine, or gitlab"
            )),
        }
    }
}

/// Resolve the provider kind for a single invocation.
///
/// Precedence, highest first:
///   1. Explicit `--provider` argument supplied by the caller.
///   2. `PHASEGENT_PROVIDER` environment variable (one-process
///      override, identical to phase 2 behaviour).
///   3. `PHASEGENT_DEFAULT_PROVIDER` environment variable
///      (one-process override for the persistent default).
///   4. Persisted `PHASEGENT_DEFAULT_PROVIDER` in the
///      `global_setting` table (machine-wide default that survives
///      across processes; surfaces during a single `resolve_kind`
///      call without touching the role-scoped config).
///   5. Role-scoped `role_config.provider` (the existing phase 2
///      behaviour).
///   6. Forgejo fallback.
///
/// Steps 1 and 2 already existed; steps 3 through 6 are added by
/// phase `global-provider-default`. The resolver is read-only: it
/// never persists anything, so a stray `--provider` omission cannot
/// silently overwrite the role-scoped or machine-wide configuration.
pub fn resolve_kind(
    role: Role,
    explicit: Option<ProviderKind>,
) -> Result<ProviderKind, ForgejoError> {
    if let Some(provider) = explicit {
        return Ok(provider);
    }
    if let Ok(provider) = std::env::var("PHASEGENT_PROVIDER") {
        return provider
            .parse()
            .map_err(|error: String| ForgejoError::config(error));
    }
    if let Ok(provider) = std::env::var("PHASEGENT_DEFAULT_PROVIDER") {
        let trimmed = provider.trim();
        if !trimmed.is_empty() {
            return trimmed
                .parse()
                .map_err(|error: String| ForgejoError::config(error));
        }
    }
    // Persisted global default lives in `global_setting`. Read it
    // directly so the resolver never writes — the schema-level
    // helpers take care of the secret-bearing fields separately.
    if let Ok(storage) = Storage::open()
        && let Ok(Some(value)) = storage.load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
    {
        return value
            .parse()
            .map_err(|error: String| ForgejoError::config(error));
    }
    let storage = Storage::open().map_err(ForgejoError::config)?;
    let stored = auth::load_config(role, &storage).map_err(ForgejoError::config)?;
    stored
        .and_then(|config| config.provider)
        .map_or(Ok(ProviderKind::Forgejo), |provider| {
            provider
                .parse()
                .map_err(|error: String| ForgejoError::config(error))
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedmineConfig {
    pub api_base: String,
    pub project_id: Option<String>,
    pub close_status_id: Option<u64>,
}

#[allow(dead_code)]
impl RedmineConfig {
    pub fn new(
        api_base: impl Into<String>,
        project_id: impl Into<String>,
        close_status_id: u64,
    ) -> Self {
        Self {
            api_base: api_base.into(),
            project_id: Some(project_id.into()),
            close_status_id: Some(close_status_id),
        }
    }

    pub const fn provider(&self) -> ProviderKind {
        ProviderKind::Redmine
    }

    pub fn resolve(
        role: Role,
        api_base: Option<&str>,
        project_id: Option<&str>,
        close_status_id: Option<&str>,
    ) -> Result<Self, ForgejoError> {
        let storage = Storage::open().map_err(ForgejoError::config)?;
        let stored = auth::load_redmine_config(role, &storage).map_err(ForgejoError::config)?;
        let explicit_base = api_base
            .map(str::to_owned)
            .or_else(|| std::env::var("PHASEGENT_REDMINE_API_BASE").ok())
            .or_else(|| std::env::var("PHASEGENT_API_BASE").ok());
        let explicit_project = project_id.map(str::to_owned);
        let explicit_close = close_status_id
            .map(str::to_owned)
            .or_else(|| std::env::var("PHASEGENT_REDMINE_CLOSE_STATUS_ID").ok())
            .or_else(|| std::env::var("PHASEGENT_CLOSE_STATUS_ID").ok());

        let base = explicit_base
            .or_else(|| stored.as_ref().and_then(|config| config.api_base.clone()))
            .ok_or_else(|| {
                ForgejoError::config(
                    "Redmine API base is not configured; use --api-base or auth setup",
                )
            })?;
        let project_id = explicit_project.filter(|value| !value.trim().is_empty());
        let close_status_id = explicit_close
            .or_else(|| {
                stored
                    .as_ref()
                    .and_then(|config| config.close_status_id)
                    .map(|value| value.to_string())
            })
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| ForgejoError::config("Redmine close status id must be numeric"))
            })
            .transpose()?;
        if close_status_id == Some(0) {
            return Err(ForgejoError::config(
                "Redmine close status id must be greater than zero",
            ));
        }

        let api_base = remote::normalize_redmine_api_base(&base).map_err(ForgejoError::config)?;
        Ok(Self {
            api_base,
            project_id,
            close_status_id,
        })
    }

    pub fn require_project_id(&self) -> Result<&str, ForgejoError> {
        self.project_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ForgejoError::config("Redmine project id is not configured; use --project-id")
            })
    }

    pub fn require_close_status_id(&self) -> Result<u64, ForgejoError> {
        match self.close_status_id {
            Some(value) if value > 0 => Ok(value),
            Some(_) => Err(ForgejoError::config(
                "Redmine close status id must be greater than zero",
            )),
            None => Err(ForgejoError::config(
                "Redmine close status id is not configured; use --close-status-id or auth setup",
            )),
        }
    }
}

#[allow(dead_code)]
pub struct RedmineProvider {
    pub(crate) config: RedmineConfig,
    pub(crate) http: RedmineHttp,
}

impl RedmineProvider {
    pub fn for_role(role: Role, config: RedmineConfig) -> Result<Self, ForgejoError> {
        let storage = Storage::open().map_err(ForgejoError::config)?;
        let api_key = auth::redmine_api_key(role, &storage).map_err(ForgejoError::auth)?;
        Self::new(config, api_key)
    }

    pub fn new(config: RedmineConfig, api_key: String) -> Result<Self, ForgejoError> {
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() {
            return Err(ForgejoError::auth("Redmine API key is empty"));
        }
        let http = RedmineHttp::new(config.api_base.clone(), api_key)?;
        Ok(Self { config, http })
    }

    fn unsupported<T>(&self, operation: &str) -> Result<T, ForgejoError> {
        Err(ForgejoError::not_supported("redmine", operation))
    }

    pub fn create_repo(
        &self,
        _target: &str,
        _private: bool,
        _description: &str,
        _auto_init: bool,
    ) -> Result<RepoSummary, ForgejoError> {
        self.unsupported("repo create")
    }

    pub fn ci_runs(&self, _filter: &CiRunsFilter) -> Result<CiRunsOutput, ForgejoError> {
        self.unsupported("ci runs")
    }

    pub fn ci_run_get(&self, _run_id: u64) -> Result<CiRunSummary, ForgejoError> {
        self.unsupported("ci run get")
    }

    pub fn ci_run_jobs(&self, _run_id: u64) -> Result<CiJobsOutput, ForgejoError> {
        self.unsupported("ci run jobs")
    }

    pub fn ci_job_logs(&self, _job_id: u64, _tail: usize) -> Result<CiJobLogsOutput, ForgejoError> {
        self.unsupported("ci job logs")
    }

    pub fn ci_inspect(&self, _request: &CiInspectRequest) -> Result<CiInspectOutput, ForgejoError> {
        self.unsupported("ci inspect")
    }
}

/// Resolved GitLab configuration. The `project_id` is a numeric GitLab
/// project identifier; the `api_base` is the `/api/v4` endpoint URL
/// already normalised by [`normalize_gitlab_api_base`]. The struct is
/// intentionally minimal in this foundation phase: subsequent phases
/// will extend it with workflow, time-tracking, and link fields without
/// disturbing the existing resolver path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitlabConfig {
    pub api_base: String,
    pub project_id: u64,
}

#[allow(dead_code)]
impl GitlabConfig {
    pub fn new(api_base: impl Into<String>, project_id: u64) -> Self {
        Self {
            api_base: api_base.into(),
            project_id,
        }
    }

    pub const fn provider(&self) -> ProviderKind {
        ProviderKind::Gitlab
    }

    /// Resolve the GitLab configuration for `role`.
    ///
    /// Resolution precedence:
    ///   1. Explicit `--api-base` / `--project-id` flags supplied by the
    ///      caller.
    ///   2. `PHASEGENT_GITLAB_API_BASE` / `PHASEGENT_API_BASE` environment
    ///      variables for the base (project-id env and persisted values
    ///      were removed in Phase 1).
    ///   3. Persisted `api_base` in `role_gitlab_config`.
    ///
    /// The project id is required as an explicit `--project-id` because
    /// GitLab workflow commands need a single, unambiguous target; an
    /// unset project id returns a structured error rather than silently
    /// selecting the wrong project.
    pub fn resolve(
        role: Role,
        api_base: Option<&str>,
        project_id: Option<&str>,
    ) -> Result<Self, ForgejoError> {
        let storage = Storage::open().map_err(ForgejoError::config)?;
        let stored = auth::load_gitlab_config(role, &storage).map_err(ForgejoError::config)?;
        let explicit_base = api_base
            .map(str::to_owned)
            .or_else(|| std::env::var("PHASEGENT_GITLAB_API_BASE").ok())
            .or_else(|| std::env::var("PHASEGENT_API_BASE").ok());
        let explicit_project = project_id.map(str::to_owned);

        let base = explicit_base
            .or_else(|| stored.as_ref().and_then(|config| config.api_base.clone()))
            .ok_or_else(|| {
                ForgejoError::config(
                    "GitLab API base is not configured; use --api-base or auth setup",
                )
            })?;
        // Project id source: only explicit `--project-id`. Env and
        // persisted values were removed in Phase 1 (remove-project-id)
        // and are intentionally ignored to ensure legacy rows are inert.
        let parsed_project: u64 = match explicit_project
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| ForgejoError::config("GitLab project id must be numeric"))
            })
            .transpose()?
        {
            Some(value) => value,
            None => {
                return Err(ForgejoError::config(
                    "GitLab project id is not configured; use --project-id",
                ));
            }
        };
        if parsed_project == 0 {
            return Err(ForgejoError::config(
                "GitLab project id must be greater than zero",
            ));
        }

        let api_base = normalize_gitlab_api_base(&base).map_err(ForgejoError::config)?;
        Ok(Self {
            api_base,
            project_id: parsed_project,
        })
    }

    /// Constrain the borrow of `api_base` for callers that need to read
    /// the normalised endpoint without consuming the config.
    pub fn require_api_base(&self) -> &str {
        &self.api_base
    }
}

/// Normalise the GitLab API base URL to its `/api/v4` endpoint while
/// preserving any deployment prefix the operator may have configured
/// (for example `https://gitlab.example/gitlab` becomes
/// `https://gitlab.example/gitlab/api/v4`).
///
/// Lives next to [`GitlabConfig`] because `remote.rs` is intentionally
/// provider-agnostic and the GitLab API path suffix is a GitLab-only
/// concern. The function accepts both `/api/v4` already present (no-op)
/// and a bare host (single append), so callers can rely on a stable
/// `…/api/v4` final path.
pub fn normalize_gitlab_api_base(value: &str) -> Result<String, String> {
    let mut url =
        Url::parse(value).map_err(|error| format!("invalid GitLab API base URL: {error}"))?;
    if url.host_str().is_none() || !matches!(url.scheme(), "http" | "https") {
        return Err("GitLab API base URL must use http or https".to_owned());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("GitLab API base URL cannot contain a query or fragment".to_owned());
    }
    let mut path = url.path().trim_end_matches('/').to_owned();
    if path.is_empty() {
        path = String::from("/api/v4");
    } else if !path.ends_with("/api/v4") {
        path.push_str("/api/v4");
    }
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

/// Re-export the real GitLab provider implementation. The stub used
/// to live in this module in Phase 1; Phase 2 moved the
/// implementation to [`crate::providers::gitlab`] so the HTTP plumbing and the
/// provider logic share a single file. Keeping the old name here
/// means every existing `crate::providers::config::GitlabProvider`
/// reference continues to compile without churn.
pub use crate::providers::gitlab::GitlabProvider;

#[cfg(test)]
mod tests {
    use super::{GitlabConfig, ProviderKind, normalize_gitlab_api_base};

    #[test]
    fn provider_kind_round_trip_includes_gitlab() {
        assert_eq!(ProviderKind::Gitlab.as_str(), "gitlab");
        assert_eq!(
            "gitlab".parse::<ProviderKind>().unwrap(),
            ProviderKind::Gitlab
        );
        let error = "wrong".parse::<ProviderKind>().unwrap_err();
        assert!(error.contains("forgejo, redmine, or gitlab"));
    }

    #[test]
    fn normalize_appends_api_v4_once_for_bare_host() {
        let value = normalize_gitlab_api_base("https://gitlab.example").unwrap();
        assert_eq!(value, "https://gitlab.example/api/v4");
    }

    #[test]
    fn normalize_preserves_deployment_prefix() {
        let value = normalize_gitlab_api_base("https://gitlab.example/gitlab").unwrap();
        assert_eq!(value, "https://gitlab.example/gitlab/api/v4");
    }

    #[test]
    fn normalize_is_idempotent_when_api_v4_already_present() {
        let value = normalize_gitlab_api_base("https://gitlab.example/api/v4").unwrap();
        assert_eq!(value, "https://gitlab.example/api/v4");
        let prefixed = normalize_gitlab_api_base("https://gitlab.example/gitlab/api/v4").unwrap();
        assert_eq!(prefixed, "https://gitlab.example/gitlab/api/v4");
    }

    #[test]
    fn normalize_rejects_invalid_schemes_and_queries() {
        assert!(normalize_gitlab_api_base("ftp://gitlab.example").is_err());
        assert!(normalize_gitlab_api_base("https://gitlab.example?token=hush").is_err());
        assert!(normalize_gitlab_api_base("https://gitlab.example#fragment").is_err());
        assert!(normalize_gitlab_api_base("not a url").is_err());
    }

    #[test]
    fn provider_rejects_empty_token() {
        let provider = crate::providers::gitlab::GitlabProvider::new(
            GitlabConfig::new("https://gitlab.example/api/v4", 7),
            "  ".to_owned(),
        );
        let error = provider.unwrap_err();
        assert!(error.to_string().contains("empty"));
    }
}
