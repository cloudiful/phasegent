use crate::providers::api::PhasegentError;
use url::Url;

/// Validated Forgejo connection settings.
///
/// `api_base` is the instance address without a `/api/v1` suffix; the HTTP
/// client appends that prefix itself. `repository` is the managed
/// `owner/repo` scope and never a URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgejoConfig {
    pub(super) api_base: String,
    pub(super) repository: String,
}

impl ForgejoConfig {
    pub fn new(
        api_base: impl Into<String>,
        repository: impl Into<String>,
    ) -> Result<Self, PhasegentError> {
        let api_base = validate_api_base(&api_base.into())?;
        let repository = validate_repository(&repository.into())?;
        Ok(Self {
            api_base,
            repository,
        })
    }

    pub fn resolve(
        role: crate::policy::Role,
        api_base: Option<&str>,
        repository: Option<&str>,
    ) -> Result<Self, PhasegentError> {
        if let (Some(base), Some(repo)) = (api_base, repository) {
            return Self::new(base, repo);
        }
        let storage = crate::infra::storage::Storage::open().map_err(PhasegentError::config)?;
        Self::resolve_with_storage(&storage, role, api_base, repository)
    }

    pub(super) fn resolve_with_storage(
        storage: &crate::infra::storage::Storage,
        role: crate::policy::Role,
        api_base: Option<&str>,
        repository: Option<&str>,
    ) -> Result<Self, PhasegentError> {
        let stored = storage
            .load_forgejo_config(role)
            .map_err(PhasegentError::config)?;
        let base = api_base
            .or_else(|| stored.as_ref().map(|config| config.api_base.as_str()))
            .ok_or_else(|| {
                PhasegentError::config(
                    "Forgejo API base is not configured; provide --api-base or operator setup",
                )
            })?;
        let repo = repository
            .or_else(|| stored.as_ref().map(|config| config.repository.as_str()))
            .ok_or_else(|| {
                PhasegentError::config(
                    "Forgejo repository is not configured; provide --repository or operator setup",
                )
            })?;
        Self::new(base, repo)
    }

    pub(crate) fn api_base(&self) -> &str {
        &self.api_base
    }
    pub(crate) fn repository(&self) -> &str {
        &self.repository
    }

    pub(crate) fn owner(&self) -> &str {
        self.repository
            .split_once('/')
            .map(|(owner, _)| owner)
            .expect("validated Forgejo repository always contains a separator")
    }

    pub(crate) fn repo(&self) -> &str {
        self.repository
            .split_once('/')
            .map(|(_, repo)| repo)
            .expect("validated Forgejo repository always contains a separator")
    }
}

/// Validate an instance base URL: http(s), a host, no userinfo, no query, no
/// fragment. A sub-path is preserved so self-hosted instances under a prefix
/// keep working.
pub(crate) fn validate_api_base(value: &str) -> Result<String, PhasegentError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(PhasegentError::config("Forgejo API base is empty"));
    }
    let mut url = Url::parse(trimmed).map_err(|error| {
        PhasegentError::config(format!("invalid Forgejo API base URL: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PhasegentError::config(
            "Forgejo API base must use the http or https scheme",
        ));
    }
    if url.host_str().is_none() {
        return Err(PhasegentError::config(
            "Forgejo API base must include a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PhasegentError::config(
            "Forgejo API base must not include userinfo",
        ));
    }
    if url.query().is_some() {
        return Err(PhasegentError::config(
            "Forgejo API base must not include a query string",
        ));
    }
    if url.fragment().is_some() {
        return Err(PhasegentError::config(
            "Forgejo API base must not include a fragment",
        ));
    }
    let path = url.path().trim_end_matches('/').to_owned();
    if path.is_empty() {
        url.set_path("");
    } else {
        url.set_path(&path);
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

/// Validate a repository scope: exactly `owner/repo`, no URL, no whitespace or
/// control characters, and only characters that are safe as one path segment.
pub(crate) fn validate_repository(value: &str) -> Result<String, PhasegentError> {
    let trimmed = value;
    if trimmed.is_empty() {
        return Err(PhasegentError::config("Forgejo repository is empty"));
    }
    if trimmed.contains("://") {
        return Err(PhasegentError::config(
            "Forgejo repository must be owner/repo, not a URL",
        ));
    }
    let mut parts = trimmed.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(invalid_repository(value));
    }
    for segment in [owner, repo] {
        if segment == "." || segment == ".." {
            return Err(invalid_repository(value));
        }
        let valid = segment.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        });
        if !valid || segment.starts_with('.') || segment.ends_with('.') {
            return Err(invalid_repository(value));
        }
    }
    Ok(format!("{owner}/{repo}"))
}

fn invalid_repository(_value: &str) -> PhasegentError {
    PhasegentError::config(
        "invalid Forgejo repository; expected owner/repo using letters, digits, '-', '_', or '.'",
    )
}
