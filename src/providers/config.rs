use crate::auth;
use crate::infra::storage::Storage;
use crate::policy::Role;
use crate::providers::api::PhasegentError;
use crate::providers::redmine::http::RedmineHttp;
use crate::remote;
use std::str::FromStr;

/// The two tracking providers phasegent supports.
///
/// `Redmine` is the configured default; `Local` is the credential-free
/// SQLite store. Any other name is rejected before a provider is
/// resolved, so a retired value can never fall back silently.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderKind {
    #[default]
    Redmine,
    /// Local provider. Recognised by the resolver, `--provider` flag
    /// parsing, and the config snapshot via `as_str`/`from_str`.
    Local,
}

impl ProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Redmine => "redmine",
            Self::Local => "local",
        }
    }

    /// Whether `value` names a provider this build still recognises,
    /// including the retired names, so callers can produce a value-aware
    /// diagnostic instead of a generic parse error. The literals come
    /// from the storage layer so the rejection and the persisted legacy
    /// rows can never drift apart.
    fn is_retired(value: &str) -> bool {
        use crate::infra::storage::{PROVIDER_FORGEJO, PROVIDER_GITLAB};
        value == PROVIDER_FORGEJO || value == PROVIDER_GITLAB
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
            retired if Self::is_retired(retired) => Err(format!(
                "tracking provider '{retired}' is no longer supported; \
                 phasegent tracks only 'redmine' and 'local'"
            )),
            _ => Err(format!(
                "invalid provider '{value}'; expected redmine or local"
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
///   4. TOML `default_provider` in `phasegent.toml` (human-editable
///      overlay; `PHASEGENT_CONFIG_PATH` isolates the path in tests).
///   5. Persisted `PHASEGENT_DEFAULT_PROVIDER` in the `global_setting`
///      table (machine-wide default that survives across processes).
///   6. Role-scoped `role_config.provider` as returned by
///      `auth::load_config` (effective TOML-over-SQLite per role).
///   7. Redmine fallback.
///
/// A retired explicit or configured name errors here, before any network
/// access, instead of falling back. The resolver is read-only: it never
/// persists anything and never writes TOML.
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
    // TOML overlay sits between env and SQLite. A malformed/secret TOML
    // fails here instead of falling back so misconfiguration is visible.
    if let Some(overlay) =
        crate::infra::config_overlay::load_overlay().map_err(PhasegentError::config)?
        && let Some(value) = overlay.default_provider_value()
    {
        return value
            .parse()
            .map_err(|error: String| PhasegentError::config(error));
    }
    // Persisted global default lives in `global_setting`. Read it
    // directly so the resolver never writes.
    if let Ok(storage) = Storage::open()
        && let Ok(Some(value)) = storage.load_global_setting("PHASEGENT_DEFAULT_PROVIDER")
    {
        return value
            .parse()
            .map_err(|error: String| PhasegentError::config(error));
    }
    let storage = Storage::open().map_err(PhasegentError::config)?;
    let stored = auth::load_config(role, &storage).map_err(PhasegentError::config)?;
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
    /// (`PHASEGENT_REDMINE_API_BASE` / `PHASEGENT_API_BASE`) > TOML
    /// `redmine_api_base` > persisted global setting > bounded legacy
    /// role migration. No role-scoped address row is consulted for the
    /// base. The close-status id stays role-scoped (explicit >
    /// `PHASEGENT_REDMINE_CLOSE_STATUS_ID` / `PHASEGENT_CLOSE_STATUS_ID`
    /// > TOML/SQLite role value).
    pub fn resolve(
        role: Role,
        api_base: Option<&str>,
        project_id: Option<&str>,
        close_status_id: Option<&str>,
    ) -> Result<Self, PhasegentError> {
        let storage = Storage::open().map_err(PhasegentError::config)?;
        let stored = auth::load_redmine_config(role, &storage).map_err(PhasegentError::config)?;
        let explicit_base = api_base.map(str::to_owned);
        let explicit_project = project_id.map(str::to_owned);
        let explicit_close = close_status_id
            .map(str::to_owned)
            .or_else(|| std::env::var("PHASEGENT_REDMINE_CLOSE_STATUS_ID").ok())
            .or_else(|| std::env::var("PHASEGENT_CLOSE_STATUS_ID").ok());

        let base = match explicit_base {
            Some(value) => value,
            None => auth::redmine_api_base(&storage)
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
        let storage = Storage::open().map_err(PhasegentError::config)?;
        let api_key = auth::redmine_api_key(role, &storage).map_err(PhasegentError::auth)?;
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
    use std::str::FromStr;

    #[test]
    fn provider_kind_round_trips_only_the_two_supported_names() {
        assert_eq!(ProviderKind::Redmine.as_str(), "redmine");
        assert_eq!(ProviderKind::Local.as_str(), "local");
        assert_eq!(ProviderKind::default(), ProviderKind::Redmine);
        assert_eq!(
            "redmine".parse::<ProviderKind>().unwrap(),
            ProviderKind::Redmine
        );
        assert_eq!(
            "local".parse::<ProviderKind>().unwrap(),
            ProviderKind::Local
        );
        assert_eq!(
            ProviderKind::from_str(ProviderKind::Local.as_str()).unwrap(),
            ProviderKind::Local
        );
        assert_eq!(format!("{}", ProviderKind::Local), "local");
    }

    #[test]
    fn a_retired_provider_name_is_rejected_with_an_actionable_error() {
        for retired in ["forgejo", "gitlab"] {
            let error = retired.parse::<ProviderKind>().unwrap_err();
            assert!(error.contains(retired), "{error}");
            assert!(error.contains("redmine"), "{error}");
            assert!(error.contains("local"), "{error}");
        }
    }

    #[test]
    fn an_unknown_provider_name_names_the_supported_set() {
        let error = "wrong".parse::<ProviderKind>().unwrap_err();
        assert_eq!(error, "invalid provider 'wrong'; expected redmine or local");
    }
}
