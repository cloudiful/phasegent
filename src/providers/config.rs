use crate::auth;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::providers::redmine::http::RedmineHttp;
use crate::remote;
use std::str::FromStr;

/// The tracking providers phasegent supports.
///
/// `Redmine` is the fallback; `Local` is the credential-free SQLite store;
/// `Forgejo` and `GitLab` are self-hosted HTTP instances. Any other name is
/// rejected before a provider is resolved.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderKind {
    #[default]
    Redmine,
    /// Local provider. Recognised by the resolver, `--provider` flag
    /// parsing, and the config snapshot via `as_str`/`from_str`.
    Local,
    Forgejo,
    Gitlab,
}

impl ProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Redmine => "redmine",
            Self::Local => "local",
            Self::Forgejo => "forgejo",
            Self::Gitlab => "gitlab",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProviderKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "redmine" => Ok(Self::Redmine),
            "local" => Ok(Self::Local),
            "forgejo" => Ok(Self::Forgejo),
            "gitlab" => Ok(Self::Gitlab),
            _ => Err(format!(
                "invalid provider '{value}'; expected redmine, local, forgejo, or gitlab"
            )),
        }
    }
}

/// Resolve the provider kind for a single invocation.
///
/// Precedence, highest first:
///   1. Explicit `--provider` argument supplied by the caller.
///   2. `PHASEGENT_PROVIDER` environment variable (one-process override).
///   3. `PHASEGENT_DEFAULT_PROVIDER` environment variable (one-process
///      override for the persistent default).
///   4. The persisted `PHASEGENT_DEFAULT_PROVIDER` in the JSONC
///      configuration file (machine-wide default that survives across
///      processes).
///   5. The role-scoped `provider` in the configuration file, as returned
///      by `auth::load_config`.
///   6. Redmine fallback.
///
/// An unknown explicit or configured name errors here, before any network
/// access, instead of falling back. The resolver is read-only: it never
/// persists anything and never writes the configuration file.
pub fn resolve_kind(
    role: Role,
    explicit: Option<ProviderKind>,
) -> Result<ProviderKind, PhasegentError> {
    if let Some(provider) = explicit {
        return Ok(provider);
    }
    if let Ok(provider) = std::env::var("PHASEGENT_PROVIDER") {
        return provider
            .parse()
            .map_err(|error: String| PhasegentError::config(error));
    }
    if let Ok(provider) = std::env::var("PHASEGENT_DEFAULT_PROVIDER") {
        let trimmed = provider.trim();
        if !trimmed.is_empty() {
            return trimmed
                .parse()
                .map_err(|error: String| PhasegentError::config(error));
        }
    }
    // The persisted machine-wide default lives in the configuration file.
    if let Some(value) =
        crate::infra::config_file::runtime::load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
            .map_err(PhasegentError::config)?
        && !value.trim().is_empty()
    {
        return value
            .parse()
            .map_err(|error: String| PhasegentError::config(error));
    }
    let stored = auth::load_config(role).map_err(PhasegentError::config)?;
    stored
        .and_then(|config| config.provider)
        .map_or(Ok(ProviderKind::Redmine), |provider| {
            provider
                .parse()
                .map_err(|error: String| PhasegentError::config(error))
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

    /// Resolve the Redmine configuration for `role`.
    ///
    /// The REST base resolves from the one canonical global address via
    /// [`auth::redmine_api_base`]: explicit `--api-base` > env
    /// (`PHASEGENT_REDMINE_API_BASE` / `PHASEGENT_API_BASE`) > the
    /// persisted global setting > bounded legacy role migration. No
    /// role-scoped address entry is consulted for the base. The
    /// close-status id stays role-scoped (explicit >
    /// `PHASEGENT_REDMINE_CLOSE_STATUS_ID` / `PHASEGENT_CLOSE_STATUS_ID`
    /// > the persisted role value).
    pub fn resolve(
        role: Role,
        api_base: Option<&str>,
        project_id: Option<&str>,
        close_status_id: Option<&str>,
    ) -> Result<Self, PhasegentError> {
        let stored = auth::load_redmine_config(role).map_err(PhasegentError::config)?;
        let explicit_base = api_base.map(str::to_owned);
        let explicit_project = project_id.map(str::to_owned);
        let explicit_close = close_status_id
            .map(str::to_owned)
            .or_else(|| std::env::var("PHASEGENT_REDMINE_CLOSE_STATUS_ID").ok())
            .or_else(|| std::env::var("PHASEGENT_CLOSE_STATUS_ID").ok());

        let base = match explicit_base {
            Some(value) => value,
            None => auth::redmine_api_base()
                .map_err(PhasegentError::config)?
                .ok_or_else(|| {
                    PhasegentError::config(
                        "Redmine API base is not configured; set the global redmine_api_base \
                         (config set redmine-api-base) or use --api-base",
                    )
                })?,
        };
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
                    .map_err(|_| PhasegentError::config("Redmine close status id must be numeric"))
            })
            .transpose()?;
        if close_status_id == Some(0) {
            return Err(PhasegentError::config(
                "Redmine close status id must be greater than zero",
            ));
        }

        let api_base = remote::normalize_redmine_api_base(&base).map_err(PhasegentError::config)?;
        Ok(Self {
            api_base,
            project_id,
            close_status_id,
        })
    }

    /// The read-only resolution used by admission and by the prune pass.
    ///
    /// It resolves the same address and close-status precedence as
    /// [`Self::resolve`], but never migrates a legacy role address into
    /// the global setting.
    pub fn resolve_read_only(
        role: Role,
        api_base: Option<&str>,
        project_id: Option<&str>,
        close_status_id: Option<&str>,
    ) -> Result<Self, PhasegentError> {
        let stored = auth::load_redmine_config(role).map_err(PhasegentError::config)?;
        let base = match api_base {
            Some(value) => value.to_owned(),
            None => auth::redmine_api_base_read_only()
                .map_err(PhasegentError::config)?
                .ok_or_else(|| {
                    PhasegentError::config(
                        "Redmine API base is not configured; set the global redmine_api_base \
                         (config set redmine-api-base) or use --api-base",
                    )
                })?,
        };
        let project_id = project_id
            .map(str::to_owned)
            .filter(|value| !value.trim().is_empty());
        let close_status_id = close_status_id
            .map(str::to_owned)
            .or_else(|| {
                std::env::var("PHASEGENT_REDMINE_CLOSE_STATUS_ID")
                    .ok()
                    .or_else(|| std::env::var("PHASEGENT_CLOSE_STATUS_ID").ok())
            })
            .or_else(|| {
                stored
                    .as_ref()
                    .and_then(|config| config.close_status_id)
                    .map(|value| value.to_string())
            })
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| PhasegentError::config("Redmine close status id must be numeric"))
            })
            .transpose()?;
        if close_status_id == Some(0) {
            return Err(PhasegentError::config(
                "Redmine close status id must be greater than zero.",
            ));
        }
        let api_base = remote::normalize_redmine_api_base(&base).map_err(PhasegentError::config)?;
        Ok(Self {
            api_base,
            project_id,
            close_status_id,
        })
    }

    pub fn require_project_id(&self) -> Result<&str, PhasegentError> {
        self.project_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                PhasegentError::config("Redmine project id is not configured; use --project-id")
            })
    }

    pub fn require_close_status_id(&self) -> Result<u64, PhasegentError> {
        match self.close_status_id {
            Some(value) if value > 0 => Ok(value),
            Some(_) => Err(PhasegentError::config(
                "Redmine close status id must be greater than zero",
            )),
            None => Err(PhasegentError::config(
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
    pub fn for_role(role: Role, config: RedmineConfig) -> Result<Self, PhasegentError> {
        let api_key = auth::redmine_api_key(role).map_err(PhasegentError::auth)?;
        Self::new(config, api_key)
    }

    pub fn new(config: RedmineConfig, api_key: String) -> Result<Self, PhasegentError> {
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() {
            return Err(PhasegentError::auth("Redmine API key is empty"));
        }
        let http = RedmineHttp::new(config.api_base.clone(), api_key)?;
        Ok(Self { config, http })
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderKind;

    #[test]
    fn provider_kind_round_trips_all_supported_names() {
        for (literal, kind) in [
            ("redmine", ProviderKind::Redmine),
            ("local", ProviderKind::Local),
            ("forgejo", ProviderKind::Forgejo),
            ("gitlab", ProviderKind::Gitlab),
        ] {
            assert_eq!(kind.as_str(), literal);
            assert_eq!(literal.parse::<ProviderKind>().unwrap(), kind);
            assert_eq!(format!("{kind}"), literal);
        }
        assert_eq!(ProviderKind::default(), ProviderKind::Redmine);
    }

    #[test]
    fn an_unknown_provider_name_names_the_supported_set() {
        let error = "wrong".parse::<ProviderKind>().unwrap_err();
        assert_eq!(
            error,
            "invalid provider 'wrong'; expected redmine, local, forgejo, or gitlab"
        );
    }
}
