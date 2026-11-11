use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::remote::GitlabOriginEvidence;
use url::Url;

/// Validated GitLab connection settings.
///
/// `api_base` is the trusted instance root without `/api/v4`; the HTTP client
/// appends the API prefix itself. `repository` is the full namespace/subgroup
/// project path used as the per-operation project selector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitlabConfig {
    pub(super) api_base: String,
    pub(super) repository: String,
}

enum RepositoryEvidence {
    OutsideGit,
    Origin(Result<GitlabOriginEvidence, String>),
}

impl GitlabConfig {
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

    /// Resolve the trusted instance and the per-operation project path from
    /// an existing store only. It never creates, migrates, or writes.
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
        stored: Option<GitlabStoredConfig>,
        evidence: RepositoryEvidence,
    ) -> Result<Self, PhasegentError> {
        let Some(stored) = stored else {
            return Err(PhasegentError::config(
                "GitLab role configuration is not present; an operator must store the trusted \
                 instance and credential with `phasegent admin auth setup --provider gitlab \
                 --target-role <ROLE>` for the intended agent role",
            ));
        };
        let trusted = if stored.api_base.trim().is_empty() {
            return Err(PhasegentError::config(
                "GitLab role configuration is incomplete: the stored instance is missing; \
                 rerun `phasegent admin auth setup --provider gitlab --target-role <ROLE>`",
            ));
        } else {
            validate_api_base(&stored.api_base)?
        };
        if let Some(selected) = api_base {
            let selected = validate_api_base(selected)?;
            if selected != trusted {
                return Err(PhasegentError::config(
                    "selected GitLab endpoint does not match the configured role instance; \
                     refusing to send the role credential",
                ));
            }
        }
        let stored_repository = stored
            .repository
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                PhasegentError::config(
                    "GitLab role configuration is incomplete: the stored project path is missing; \
                     rerun `phasegent admin auth setup --provider gitlab --target-role <ROLE>`",
                )
            })?;
        let repository = match repository {
            Some(repository) => validate_repository(repository)?,
            None => match evidence {
                RepositoryEvidence::OutsideGit => validate_repository(stored_repository)?,
                RepositoryEvidence::Origin(Ok(origin)) => {
                    attest_origin_repository(&trusted, &origin)?
                }
                RepositoryEvidence::Origin(Err(reason)) => {
                    return Err(PhasegentError::config(format!(
                        "cannot select a GitLab project from the current checkout: {reason}; \
                         provide --repository to name the full project path on the trusted instance"
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

    pub(crate) fn attest_origin_repository(
        trusted_api_base: &str,
        origin: &GitlabOriginEvidence,
    ) -> Result<String, PhasegentError> {
        attest_origin_repository(trusted_api_base, origin)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GitlabStoredConfig {
    pub api_base: String,
    pub repository: Option<String>,
}

fn stored_config(
    storage: &Storage,
    role: Role,
) -> Result<Option<GitlabStoredConfig>, PhasegentError> {
    if !storage
        .table_exists("role_gitlab_config")
        .map_err(PhasegentError::config)?
        || !storage
            .table_has_column("role_gitlab_config", "api_base")
            .map_err(PhasegentError::config)?
        || !storage
            .table_has_column("role_gitlab_config", "repository")
            .map_err(PhasegentError::config)?
    {
        return Ok(None);
    }
    Ok(storage
        .load_gitlab_config(role)
        .map_err(PhasegentError::config)?
        .map(|config| GitlabStoredConfig {
            api_base: config.api_base,
            repository: config.repository,
        }))
}

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
        crate::remote::gitlab_origin_evidence(),
    ))
}

/// The origin proves the full project path only when its host and deployment
/// prefix equal the trusted instance's. Scheme and port never participate, and
/// the endpoint stays the trusted instance.
fn attest_origin_repository(
    trusted_api_base: &str,
    origin: &GitlabOriginEvidence,
) -> Result<String, PhasegentError> {
    let trusted = Url::parse(trusted_api_base)
        .map_err(|error| PhasegentError::config(format!("invalid GitLab instance: {error}")))?;
    let trusted_host = trusted.host_str().unwrap_or_default();
    if !trusted_host.eq_ignore_ascii_case(&origin.host) {
        return Err(PhasegentError::config(format!(
            "origin host '{}' does not attest the configured GitLab instance '{}'; \
             provide --repository to select the full project path on the trusted instance",
            origin.host, trusted_host
        )));
    }
    let trusted_prefix = trusted.path().trim_end_matches('/');
    let project = if trusted_prefix.is_empty() {
        origin.path.to_owned()
    } else {
        let prefix = format!("{trusted_prefix}/");
        let origin_path = format!("/{}", origin.path);
        origin_path
            .strip_prefix(&prefix)
            .ok_or_else(|| {
                PhasegentError::config(format!(
                    "origin path '{}' does not attest the configured GitLab instance prefix \
                     '{trusted_prefix}'; provide --repository to select the full project path on \
                     the trusted instance",
                    origin.path
                ))
            })?
            .to_owned()
    };
    validate_repository(&project).map_err(|_| {
        PhasegentError::config(format!(
            "origin path '{}' does not name a full GitLab namespace/project on the trusted \
             instance; provide --repository",
            origin.path
        ))
    })
}

/// Validate an instance root: http(s), a host, no userinfo, no query, no
/// fragment, and no `/api/v4` suffix. A deployment sub-path is preserved.
pub(crate) fn validate_api_base(value: &str) -> Result<String, PhasegentError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(PhasegentError::config("GitLab API base is empty"));
    }
    let mut url = Url::parse(trimmed)
        .map_err(|error| PhasegentError::config(format!("invalid GitLab API base URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PhasegentError::config(
            "GitLab API base must use the http or https scheme",
        ));
    }
    if url.host_str().is_none() {
        return Err(PhasegentError::config(
            "GitLab API base must include a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PhasegentError::config(
            "GitLab API base must not include userinfo",
        ));
    }
    if url.query().is_some() {
        return Err(PhasegentError::config(
            "GitLab API base must not include a query string",
        ));
    }
    if url.fragment().is_some() {
        return Err(PhasegentError::config(
            "GitLab API base must not include a fragment",
        ));
    }
    let path = url.path().trim_end_matches('/').to_owned();
    if path.ends_with("/api/v4") {
        return Err(PhasegentError::config(
            "GitLab API base must be the instance root without /api/v4",
        ));
    }
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

/// Validate a full project path: at least a namespace and a project segment,
/// no URL, no whitespace or control characters, and only path-safe segment
/// characters. Subgroups are preserved verbatim and never mistaken for a
/// deployment prefix.
pub(crate) fn validate_repository(value: &str) -> Result<String, PhasegentError> {
    if value.is_empty() {
        return Err(PhasegentError::config("GitLab project path is empty"));
    }
    if value.contains("://") {
        return Err(PhasegentError::config(
            "GitLab project path must be namespace/project, not a URL",
        ));
    }
    let parts: Vec<&str> = value.split('/').collect();
    if parts.len() < 2 {
        return Err(invalid_repository());
    }
    for segment in parts {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.starts_with('.')
            || segment.ends_with('.')
        {
            return Err(invalid_repository());
        }
        let valid = segment.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        });
        if !valid {
            return Err(invalid_repository());
        }
    }
    Ok(value.to_owned())
}

fn invalid_repository() -> PhasegentError {
    PhasegentError::config(
        "invalid GitLab project path; expected namespace/project (subgroups allowed) using \
         letters, digits, '-', '_', or '.'",
    )
}

#[cfg(test)]
mod tests {
    use super::{GitlabConfig, validate_api_base, validate_repository};
    use crate::remote::GitlabOriginEvidence;

    fn origin(path: &str) -> GitlabOriginEvidence {
        GitlabOriginEvidence {
            host: "gitlab.example.com".to_owned(),
            path: path.to_owned(),
        }
    }

    #[test]
    fn instance_root_rejects_api_suffix_and_keeps_deployment_prefix() {
        assert_eq!(
            validate_api_base("https://gitlab.example.com/root/").unwrap(),
            "https://gitlab.example.com/root"
        );
        for rejected in [
            "https://gitlab.example.com/api/v4",
            "https://user:secret@gitlab.example.com",
            "https://gitlab.example.com?x=1",
            "https://gitlab.example.com#frag",
            "ftp://gitlab.example.com",
        ] {
            assert!(validate_api_base(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn project_path_requires_two_safe_segments_and_keeps_subgroups() {
        assert_eq!(
            validate_repository("group/sub/project").unwrap(),
            "group/sub/project"
        );
        for rejected in [
            "project",
            "/group/project",
            "group//project",
            "group/../project",
            "group/.hidden",
            "https://gitlab.example.com/group/project",
            "group/pro ject",
        ] {
            assert!(validate_repository(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn origin_attestation_separates_deployment_prefix_from_subgroups() {
        let plain = GitlabConfig::attest_origin_repository(
            "https://gitlab.example.com",
            &origin("group/sub/project"),
        )
        .unwrap();
        assert_eq!(plain, "group/sub/project");

        let prefixed = GitlabConfig::attest_origin_repository(
            "https://gitlab.example.com/gitlab",
            &origin("gitlab/group/sub/project"),
        )
        .unwrap();
        assert_eq!(prefixed, "group/sub/project");

        assert!(
            GitlabConfig::attest_origin_repository(
                "https://gitlab.example.com/gitlab",
                &origin("group/sub/project"),
            )
            .is_err(),
            "a missing deployment prefix cannot be read as a subgroup"
        );
        assert!(
            GitlabConfig::attest_origin_repository(
                "https://gitlab.example.com",
                &origin("gitlab/group/project"),
            )
            .is_ok(),
            "without a configured prefix, the leading path segment is a subgroup"
        );
        let other_host = GitlabOriginEvidence {
            host: "other.example.com".to_owned(),
            path: "group/project".to_owned(),
        };
        assert!(
            GitlabConfig::attest_origin_repository("https://gitlab.example.com", &other_host)
                .is_err()
        );
    }

    #[test]
    fn constructor_rejects_a_url_as_project_path() {
        assert!(GitlabConfig::new("https://gitlab.example.com", "group/project").is_ok());
        assert!(
            GitlabConfig::new(
                "https://gitlab.example.com",
                "https://gitlab.example.com/x/y"
            )
            .is_err()
        );
    }
}
