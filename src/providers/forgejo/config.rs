use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::remote::OriginRepositoryEvidence;
use url::Url;

/// Validated Forgejo connection settings.
///
/// `api_base` is the trusted instance address without a `/api/v1` suffix; the
/// HTTP client appends that prefix itself. `repository` is the per-operation
/// `owner/repo` scope and never a URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgejoConfig {
    pub(super) api_base: String,
    pub(super) repository: String,
}

/// Where an implicit repository selection may come from: only outside Git is
/// the stored repository a default; inside Git the current origin must attest
/// the trusted instance or the selection refuses.
enum RepositoryEvidence {
    OutsideGit,
    Origin(Result<OriginRepositoryEvidence, String>),
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

    /// Resolve the trusted instance and the per-operation repository without
    /// creating or migrating storage. The role row supplies the trusted
    /// instance and the outside-Git repository default; the current origin is
    /// evidence only and never selects the instance or the endpoint.
    pub fn resolve(
        role: Role,
        api_base: Option<&str>,
        repository: Option<&str>,
    ) -> Result<Self, PhasegentError> {
        let storage = Storage::open_existing_read_only().map_err(PhasegentError::config)?;
        match storage.as_ref() {
            Some(storage) => Self::resolve_with_storage(storage, role, api_base, repository),
            None => Self::select(api_base, repository, None, repository_evidence(repository)?),
        }
    }

    /// The test-facing storage variant of [`Self::resolve`]: the same
    /// instance/repository selection over a supplied store. Reads only.
    pub(super) fn resolve_with_storage(
        storage: &Storage,
        role: Role,
        api_base: Option<&str>,
        repository: Option<&str>,
    ) -> Result<Self, PhasegentError> {
        let stored = stored_config(storage, role)?;
        Self::select(
            api_base,
            repository,
            stored,
            repository_evidence(repository)?,
        )
    }

    fn select(
        api_base: Option<&str>,
        repository: Option<&str>,
        stored: Option<(String, String)>,
        evidence: RepositoryEvidence,
    ) -> Result<Self, PhasegentError> {
        let Some((stored_api_base, stored_repository)) = stored else {
            return Err(PhasegentError::config(
                "Forgejo role configuration is not present; an operator must store the trusted \
                 instance and credential with `phasegent admin auth setup --provider forgejo \
                 --target-role <ROLE>` for the intended agent role",
            ));
        };
        let trusted = validate_api_base(&stored_api_base)?;
        if let Some(selected) = api_base {
            let selected = validate_api_base(selected)?;
            if selected != trusted {
                return Err(PhasegentError::config(
                    "selected Forgejo endpoint does not match the configured role instance; \
                     refusing to send the role credential",
                ));
            }
        }
        let repository = match repository {
            Some(repository) => validate_repository(repository)?,
            None => match evidence {
                RepositoryEvidence::OutsideGit => validate_repository(&stored_repository)?,
                RepositoryEvidence::Origin(Ok(origin)) => {
                    attest_origin_repository(&trusted, &origin)?
                }
                RepositoryEvidence::Origin(Err(reason)) => {
                    return Err(PhasegentError::config(format!(
                        "cannot select a Forgejo repository from the current checkout: {reason}; \
                         provide --repository to name the repository on the trusted instance"
                    )));
                }
            },
        };
        Self::new(trusted, repository)
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

/// The role's stored trusted instance and repository, if the existing store
/// already has the required table and columns. Missing or older schemas are
/// absence, never an excuse to migrate during selection.
fn stored_config(
    storage: &Storage,
    role: Role,
) -> Result<Option<(String, String)>, PhasegentError> {
    if !storage
        .table_exists("role_forgejo_config")
        .map_err(PhasegentError::config)?
        || !storage
            .table_has_column("role_forgejo_config", "api_base")
            .map_err(PhasegentError::config)?
        || !storage
            .table_has_column("role_forgejo_config", "repository")
            .map_err(PhasegentError::config)?
    {
        return Ok(None);
    }
    Ok(storage
        .load_forgejo_config(role)
        .map_err(PhasegentError::config)?
        .map(|config| (config.api_base, config.repository)))
}

/// Origin evidence is needed only for an implicit repository selection. An
/// explicit repository skips Git entirely; inside Git an implicit selection
/// must prove a unique origin, and outside Git the stored default is used.
fn repository_evidence(repository: Option<&str>) -> Result<RepositoryEvidence, PhasegentError> {
    if repository.is_some() {
        return Ok(RepositoryEvidence::OutsideGit);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let inside_work_tree =
        crate::worktree::is_inside_work_tree(&crate::worktree::ProcessWorktreeRunner::new(), &cwd)
            .map_err(|error| PhasegentError::config(error.message))?;
    if !inside_work_tree {
        return Ok(RepositoryEvidence::OutsideGit);
    }
    Ok(RepositoryEvidence::Origin(
        crate::remote::forgejo_origin_evidence(),
    ))
}

/// The origin repository is usable only when its normalized host and
/// deployment prefix equal the trusted instance's. Scheme and port never
/// participate, and the endpoint is always the trusted instance.
fn attest_origin_repository(
    trusted_api_base: &str,
    origin: &OriginRepositoryEvidence,
) -> Result<String, PhasegentError> {
    let trusted = Url::parse(trusted_api_base)
        .map_err(|error| PhasegentError::config(format!("invalid Forgejo instance: {error}")))?;
    let trusted_host = trusted.host_str().unwrap_or_default();
    let trusted_prefix = trusted.path().trim_end_matches('/');
    if !trusted_host.eq_ignore_ascii_case(&origin.host) {
        return Err(PhasegentError::config(format!(
            "origin host '{}' does not attest the configured Forgejo instance '{}'; \
             provide --repository to select the repository on the trusted instance",
            origin.host, trusted_host
        )));
    }
    if trusted_prefix != origin.prefix {
        return Err(PhasegentError::config(format!(
            "origin deployment prefix '{}' does not attest the configured Forgejo instance \
             prefix '{}'; provide --repository to select the repository on the trusted instance",
            origin.prefix, trusted_prefix
        )));
    }
    validate_repository(&origin.repository)
}

impl ForgejoConfig {
    /// The same origin attestation, exposed on the configuration type so a
    /// caller outside this module can compare a candidate's physical origin
    /// against a trusted instance without duplicating the rule.
    pub(crate) fn attest_origin_repository(
        trusted_api_base: &str,
        origin: &OriginRepositoryEvidence,
    ) -> Result<String, PhasegentError> {
        attest_origin_repository(trusted_api_base, origin)
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
