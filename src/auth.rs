use crate::policy::Role;
use crate::storage::{
    GLOBAL_REDMINE_GIT_MIRROR_API_KEY, GLOBAL_REDMINE_REPOSITORY_URL, PROVIDER_FORGEJO,
    PROVIDER_GITLAB, PROVIDER_REDMINE, Storage,
};
use serde::{Deserialize, Serialize};
use std::io::{self, Read};

// The canonical configuration structs live here so existing
// `auth::StoredConfig` / `auth::RedmineStoredConfig` call sites keep
// compiling and the legacy `group_name` / `group_role` JSON fields
// remain decodable for backward compatibility.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct StoredConfig {
    #[serde(default)]
    pub provider: Option<String>,
    pub api_base: Option<String>,
    pub repository: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RedmineStoredConfig {
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub close_status_id: Option<u64>,
    /// Legacy `AI Agents` group name. Preserved for backward-compatible JSON
    /// decoding of older config files; active bootstrap orchestration no
    /// longer reads or writes this field.
    #[serde(default)]
    pub group_name: Option<String>,
    /// Legacy role assigned to the `AI Agents` group. Preserved for
    /// backward-compatible JSON decoding; active bootstrap orchestration no
    /// longer reads or writes this field.
    #[serde(default)]
    pub group_role: Option<String>,
}

/// GitLab-only persistent configuration. The numeric `project_id` is the
/// GitLab project identifier; the `api_base` is the URL of the
/// `/api/v4` endpoint. Kept on a separate struct so legacy Redmine JSON
/// files never accidentally bind the wrong fields and the storage layer
/// can persist the numeric id without re-encoding a slug string.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct GitlabStoredConfig {
    #[serde(default)]
    pub api_base: Option<String>,
    /// GitLab uses numeric project ids; the field holds `Option<u64>` so
    /// the "row absent" and "row present but missing the column"
    /// semantics remain distinguishable.
    #[serde(default)]
    pub project_id: Option<u64>,
}

pub struct SetupOptions {
    pub read_stdin: bool,
    pub api_base: Option<String>,
    pub repository: Option<String>,
    pub project_id: Option<String>,
    pub close_status_id: Option<String>,
}

#[allow(dead_code)]
pub fn setup(
    role: Role,
    read_stdin: bool,
    api_base: Option<String>,
    repository: Option<String>,
) -> Result<serde_json::Value, String> {
    setup_provider(
        role,
        PROVIDER_FORGEJO,
        SetupOptions {
            read_stdin,
            api_base,
            repository,
            project_id: None,
            close_status_id: None,
        },
    )
}

pub fn setup_provider(
    role: Role,
    provider: &str,
    options: SetupOptions,
) -> Result<serde_json::Value, String> {
    let SetupOptions {
        read_stdin,
        api_base,
        repository,
        project_id,
        close_status_id,
    } = options;
    validate_provider_options(provider, &repository, &project_id, &close_status_id)?;
    let credential_label = match provider {
        PROVIDER_FORGEJO => "Forgejo token",
        PROVIDER_REDMINE => "Redmine API key",
        PROVIDER_GITLAB => "GitLab PRIVATE-TOKEN",
        _ => return Err(format!("unsupported provider '{provider}'")),
    };
    let credential = read_credential(provider, credential_label, read_stdin)?;
    let credential = credential.trim().to_owned();
    if credential.is_empty() {
        return Err(match provider {
            PROVIDER_FORGEJO => "token cannot be empty".to_owned(),
            PROVIDER_REDMINE => "Redmine API key cannot be empty".to_owned(),
            PROVIDER_GITLAB => "GitLab PRIVATE-TOKEN cannot be empty".to_owned(),
            _ => unreachable!("provider was validated above"),
        });
    }

    let storage = Storage::open()?;
    storage.save_credential(role, provider, &credential)?;

    match provider {
        PROVIDER_FORGEJO => save_forgejo_config(&storage, role, api_base, repository)?,
        PROVIDER_REDMINE => {
            save_redmine_config(&storage, role, api_base, project_id, close_status_id)?
        }
        PROVIDER_GITLAB => {
            if let Some(project_id) = project_id.as_deref().and_then(parse_gitlab_project_id) {
                persist_gitlab_bootstrap(role, api_base, project_id, &storage)?;
            } else {
                save_gitlab_config(&storage, role, api_base, project_id)?;
            }
        }
        _ => unreachable!("provider was validated above"),
    }

    if provider == PROVIDER_FORGEJO {
        Ok(serde_json::json!({
            "configured": true,
            "role": role.as_str()
        }))
    } else {
        Ok(serde_json::json!({
            "configured": true,
            "role": role.as_str(),
            "provider": provider
        }))
    }
}

fn validate_provider_options(
    provider: &str,
    repository: &Option<String>,
    project_id: &Option<String>,
    close_status_id: &Option<String>,
) -> Result<(), String> {
    if provider == "forgejo" && (project_id.is_some() || close_status_id.is_some()) {
        return Err("--project-id and --close-status-id require the redmine provider".to_owned());
    }
    if provider == "redmine" && repository.is_some() {
        return Err("--repository requires the forgejo provider".to_owned());
    }
    // GitLab reuses the Redmine flag namespace for the project id (a
    // numeric GitLab identifier instead of a Redmine slug) so the CLI
    // layer can keep a single `--project-id` flag; the forgejo
    // `repository` and redmine `close_status_id` flags do not apply.
    if provider == "gitlab" && repository.is_some() {
        return Err("--repository requires the forgejo provider".to_owned());
    }
    if provider == "gitlab" && close_status_id.is_some() {
        return Err("--close-status-id requires the redmine provider".to_owned());
    }
    Ok(())
}

fn read_credential(provider: &str, label: &str, read_stdin: bool) -> Result<String, String> {
    // GitLab PRIVATE-TOKENs are still bearer-style secrets; the label is
    // already disambiguated above. The kind is used only for the
    // rpassword prompt path so the prompt and stdin path read alike.
    let credential_kind = if provider == "forgejo" {
        "token"
    } else if provider == "gitlab" {
        "PRIVATE-TOKEN"
    } else {
        "credential"
    };
    if read_stdin {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| format!("could not read {credential_kind} from stdin: {error}"))?;
        Ok(input.trim().to_owned())
    } else {
        rpassword::prompt_password(format!("{label}: "))
            .map_err(|error| format!("could not read {credential_kind} securely: {error}"))
            .map(|value| value.trim().to_owned())
    }
}

pub fn token(role: Role, storage: &Storage) -> Result<String, String> {
    let value = storage
        .load_credential(role, PROVIDER_FORGEJO)?
        .ok_or_else(|| format!("could not read {} token: missing", role.as_str()))?;
    if value.is_empty() {
        return Err(format!("{} token is empty", role.as_str()));
    }
    Ok(value)
}

pub fn load_config(role: Role, storage: &Storage) -> Result<Option<StoredConfig>, String> {
    storage.load_role_config(role)
}

pub fn load_redmine_config(
    role: Role,
    storage: &Storage,
) -> Result<Option<RedmineStoredConfig>, String> {
    storage.load_redmine_config(role)
}

pub fn load_gitlab_config(
    role: Role,
    storage: &Storage,
) -> Result<Option<GitlabStoredConfig>, String> {
    storage.load_gitlab_config(role)
}

pub fn persist_redmine_bootstrap(
    role: Role,
    api_base: Option<String>,
    project_id: u64,
    close_status_id: u64,
    storage: &Storage,
) -> Result<(), String> {
    storage.persist_redmine_bootstrap(role, api_base, project_id, close_status_id)
}

/// Persist GitLab bootstrap identity (api_base + numeric project id)
/// for `role`. The provider preference flips to "gitlab" so subsequent
/// provider resolution picks the GitLab branch.
pub fn persist_gitlab_bootstrap(
    role: Role,
    api_base: Option<String>,
    project_id: u64,
    storage: &Storage,
) -> Result<(), String> {
    storage.persist_gitlab_bootstrap(role, api_base, project_id)
}

pub fn redmine_api_key(role: Role, storage: &Storage) -> Result<String, String> {
    let value = storage
        .load_credential(role, PROVIDER_REDMINE)?
        .ok_or_else(|| "could not read Redmine API key: missing".to_owned())?;
    if value.is_empty() {
        return Err("Redmine API key is empty".to_owned());
    }
    Ok(value)
}

/// Read the GitLab PRIVATE-TOKEN stored for `role`.
///
/// Mirrors `redmine_api_key` for symmetry with the rest of the auth
/// surface. Empty values produce a structured error so a noisy
/// `auth setup` run never silently returns an empty bearer key. The
/// token value is never surfaced in error messages; callers receive
/// only the typed error.
pub fn gitlab_token(role: Role, storage: &Storage) -> Result<String, String> {
    let value = storage
        .load_credential(role, PROVIDER_GITLAB)?
        .ok_or_else(|| "could not read GitLab PRIVATE-TOKEN: missing".to_owned())?;
    if value.is_empty() {
        return Err("GitLab PRIVATE-TOKEN is empty".to_owned());
    }
    Ok(value)
}

/// Resolve the `redmine_git_mirror` plugin bearer key at use time.
///
/// Precedence is `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` (environment)
/// → SQLite `global_setting` row → absent. Operators persist the value
/// to SQLite via `phasegent --role <ROLE> config import-env` so a
/// long-lived deployment does not have to ship the key in every shell
/// that runs `workflow bootstrap`. The environment variable still wins
/// for one-off rotations because the resolver only falls back when the
/// env var is unset or empty.
///
/// Returns `Ok(None)` when neither source yields a non-empty trimmed
/// string so callers can decide whether registration is optional or
/// required. Returning `Ok(Some)` only when the value is a non-empty
/// trimmed string keeps the value out of error messages, JSON output,
/// and test fixtures. The caller supplies the [`Storage`] handle so
/// production code can call [`Storage::open`] while tests can drive
/// the resolver against an isolated temp database.
pub fn redmine_git_mirror_api_key(storage: &Storage) -> Result<Option<String>, String> {
    if let Some(value) = read_env_trimmed("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")? {
        return Ok(Some(value));
    }
    storage
        .load_global_setting(GLOBAL_REDMINE_GIT_MIRROR_API_KEY)
        .map_err(|error| {
            format!(
                "could not read persisted Redmine git mirror key from SQLite: {error}; \
                 set PHASEGENT_REDMINE_GIT_MIRROR_API_KEY in the environment"
            )
        })
}

/// Optional override for the repository URL passed to the mirror plugin.
///
/// Precedence is `PHASEGENT_REDMINE_REPOSITORY_URL` (environment) →
/// SQLite `global_setting` row → absent. Persisting the URL is done
/// with `phasegent --role <ROLE> config import-env` so a long-lived
/// deployment does not have to ship the URL in every shell that runs
/// `workflow bootstrap`. The environment variable still wins so ad-hoc
/// runs can override the persisted URL without rewriting the database.
/// The caller supplies the [`Storage`] handle so production code can
/// call [`Storage::open`] while tests can drive the resolver against
/// an isolated temp database.
pub fn redmine_repository_url_override(storage: &Storage) -> Result<Option<String>, String> {
    if let Some(value) = read_env_trimmed("PHASEGENT_REDMINE_REPOSITORY_URL")? {
        return Ok(Some(value));
    }
    storage
        .load_global_setting(GLOBAL_REDMINE_REPOSITORY_URL)
        .map_err(|error| {
            format!("could not read persisted Redmine repository URL from SQLite: {error}")
        })
}

/// Read an environment variable and return its non-empty trimmed value
/// as `Some`. Surfaces every `VarError` other than `NotPresent` so a
/// true environment read error is not silently swallowed by the
/// SQLite fallback.
fn read_env_trimmed(name: &str) -> Result<Option<String>, String> {
    let value = match std::env::var(name) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(error) => return Err(format!("could not read {name}: {error}")),
    };
    let trimmed = value.trim().to_owned();
    Ok((!trimmed.is_empty()).then_some(trimmed))
}

fn save_forgejo_config(
    storage: &Storage,
    role: Role,
    api_base: Option<String>,
    repository: Option<String>,
) -> Result<(), String> {
    if api_base.is_none() && repository.is_none() {
        let current = storage.load_role_config(role)?;
        if current.as_ref().and_then(|c| c.provider.as_deref()) != Some(PROVIDER_REDMINE) {
            return Ok(());
        }
        return storage.update_provider(role, PROVIDER_FORGEJO);
    }
    let mut config = storage.load_role_config(role)?.unwrap_or_default();
    config.provider = Some(PROVIDER_FORGEJO.to_owned());
    if api_base.is_some() {
        config.api_base = api_base;
    }
    if repository.is_some() {
        config.repository = repository;
    }
    storage.save_role_config(role, &config)
}

fn save_redmine_config(
    storage: &Storage,
    role: Role,
    api_base: Option<String>,
    project_id: Option<String>,
    close_status_id: Option<String>,
) -> Result<(), String> {
    if api_base.is_some() || project_id.is_some() || close_status_id.is_some() {
        let mut config = storage.load_redmine_config(role)?.unwrap_or_default();
        if api_base.is_some() {
            config.api_base = api_base;
        }
        if project_id.is_some() {
            config.project_id = project_id;
        }
        if let Some(value) = close_status_id {
            config.close_status_id = Some(
                value
                    .parse()
                    .map_err(|_| "Redmine close status id must be numeric".to_owned())?,
            );
        }
        storage.save_redmine_config(role, &config)?;
    }
    storage.update_provider(role, PROVIDER_REDMINE)
}

/// Save the GitLab configuration side-effects for `auth setup`. The
/// numeric `project_id` is required because GitLab workflow commands
/// need an unambiguous target; refusing an empty value here means the
/// CLI surface can call `GitlabConfig::require_project_id` without a
/// separate "configured but missing" branch.
///
/// The check runs before storage is touched so a misuse surfaces as a
/// usage error rather than a half-written SQLite row.
fn parse_gitlab_project_id(value: &str) -> Option<u64> {
    value
        .trim()
        .parse()
        .ok()
        .filter(|project_id| *project_id > 0)
}

fn save_gitlab_config(
    storage: &Storage,
    role: Role,
    api_base: Option<String>,
    project_id: Option<String>,
) -> Result<(), String> {
    let parsed_project_id = match project_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => Some(
            value
                .parse::<u64>()
                .map_err(|_| "GitLab project id must be numeric".to_owned())?,
        ),
        None => None,
    };
    if parsed_project_id == Some(0) {
        return Err("GitLab project id must be greater than zero".to_owned());
    }
    if let Some(value) = parsed_project_id {
        storage.persist_gitlab_bootstrap(role, api_base, value)?;
        return Ok(());
    }
    if api_base.is_some() {
        let mut config = storage.load_gitlab_config(role)?.unwrap_or_default();
        config.api_base = api_base;
        storage.save_gitlab_config(role, &config)?;
    }
    storage.update_provider(role, PROVIDER_GITLAB)
}
