use crate::infra::storage::{
    GLOBAL_REDMINE_GIT_MIRROR_API_KEY, GLOBAL_REDMINE_REPOSITORY_URL, PROVIDER_FORGEJO,
    PROVIDER_GITLAB, PROVIDER_REDMINE, Storage,
};
// `PROVIDER_LOCAL` is imported from `storage_schema` directly: the
// `storage` aggregator re-export is owned by a later phase (P2/P3) and
// stays untouched in P1.
use crate::infra::storage_schema::PROVIDER_LOCAL;
use crate::policy::Role;
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
    /// Legacy Redmine project identifier. Preserved for backward-compatible
    /// JSON and SQLite decoding; Phase 1 (remove-project-id) no longer
    /// persists or reads this field—resolution uses only explicit
    /// `--project-id`. The SQLite column remains for non-destructive
    /// migration but values are ignored and cleared on open.
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
    /// Legacy GitLab project identifier. Preserved for backward-compatible
    /// JSON and SQLite decoding; Phase 1 no longer persists or reads this
    /// field—resolution uses only explicit `--project-id`. The SQLite
    /// column remains but values are ignored and cleared on open.
    #[serde(default)]
    pub project_id: Option<u64>,
}

pub struct SetupOptions {
    pub read_stdin: bool,
    pub api_base: Option<String>,
    pub repository: Option<String>,
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
        close_status_id,
    } = options;
    validate_provider_options(provider, &repository, &close_status_id)?;
    if provider == PROVIDER_LOCAL {
        // Issue 211 P1: the local provider keeps no credential, needs no
        // repository and no close-status-id (both rejected above), and
        // has no backend table yet (P4). Flip the role-scoped provider
        // preference only so `resolve_kind` and `config show` report
        // `local` while forgejo/redmine/gitlab rows stay intact.
        // `api_base`/`read_stdin` are inert here: there is nowhere to
        // persist a base URL yet and nothing to read from stdin.
        let storage = Storage::open()?;
        storage.update_provider(role, PROVIDER_LOCAL)?;
        return Ok(serde_json::json!({
            "configured": true,
            "role": role.as_str(),
            "provider": provider
        }));
    }
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
        PROVIDER_REDMINE => save_redmine_config(&storage, role, api_base, close_status_id)?,
        PROVIDER_GITLAB => {
            save_gitlab_config(&storage, role, api_base)?;
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
    close_status_id: &Option<String>,
) -> Result<(), String> {
    // Provider-agnostic: the option-applicability rules reference the
    // shared PROVIDER_* identity constants (never inline literals) so
    // the messages describe which provider owns each option rather than
    // which provider was configured. Credentials are not validated here;
    // the `setup_provider` local arm skips credential handling entirely.
    if provider == PROVIDER_FORGEJO && close_status_id.is_some() {
        return Err("--close-status-id requires the redmine provider".to_owned());
    }
    if provider == PROVIDER_REDMINE && repository.is_some() {
        return Err("--repository requires the forgejo provider".to_owned());
    }
    if provider == PROVIDER_GITLAB && repository.is_some() {
        return Err("--repository requires the forgejo provider".to_owned());
    }
    if provider == PROVIDER_GITLAB && close_status_id.is_some() {
        return Err("--close-status-id requires the redmine provider".to_owned());
    }
    // Issue 211 P1: the local provider takes neither a Forgejo
    // repository nor a Redmine close-status-id, mirroring the GitLab
    // arms above so inapplicable options fail fast instead of being
    // silently ignored.
    if provider == PROVIDER_LOCAL && repository.is_some() {
        return Err("--repository requires the forgejo provider".to_owned());
    }
    if provider == PROVIDER_LOCAL && close_status_id.is_some() {
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
    // Effective role config: TOML overlays legacy SQLite so direct file
    // edits affect the same resolver paths (Forgejo/Redmine/GitLab) used
    // by normal commands. Precedence for each field is TOML > SQLite;
    // callers apply explicit CLI > env before this stored-effective value.
    // Credentials never consult TOML. Overlay parse/secret errors propagate
    // instead of falling back so malformed TOML cannot be silently ignored.
    let base = storage.load_role_config(role)?;
    let overlay = crate::infra::config_overlay::load_overlay()?;
    let Some(overlay) = overlay else {
        return Ok(base);
    };
    let Some(role_overlay) = overlay.role_overlay(role) else {
        return Ok(base);
    };
    let mut merged = base.clone().unwrap_or_default();
    let mut present = base.is_some();
    if let Some(value) = &role_overlay.provider {
        merged.provider = Some(value.clone());
        present = true;
    }
    if let Some(value) = &role_overlay.forgejo_api_base {
        merged.api_base = Some(value.clone());
        present = true;
    }
    if let Some(value) = &role_overlay.forgejo_repository {
        merged.repository = Some(value.clone());
        present = true;
    }
    if present { Ok(Some(merged)) } else { Ok(None) }
}

pub fn load_redmine_config(
    role: Role,
    storage: &Storage,
) -> Result<Option<RedmineStoredConfig>, String> {
    // Effective Redmine config: TOML > SQLite, same contract as above.
    let base = storage.load_redmine_config(role)?;
    let overlay = crate::infra::config_overlay::load_overlay()?;
    let Some(overlay) = overlay else {
        return Ok(base);
    };
    let Some(role_overlay) = overlay.role_overlay(role) else {
        return Ok(base);
    };
    let mut merged = base.clone().unwrap_or_default();
    let mut present = base.is_some();
    if let Some(value) = &role_overlay.redmine_api_base {
        merged.api_base = Some(value.clone());
        present = true;
    }
    if let Some(value) = role_overlay.redmine_close_status_id {
        merged.close_status_id = Some(value);
        present = true;
    }
    if present { Ok(Some(merged)) } else { Ok(None) }
}

pub fn load_gitlab_config(
    role: Role,
    storage: &Storage,
) -> Result<Option<GitlabStoredConfig>, String> {
    // Effective GitLab config: TOML > SQLite, same contract as above.
    let base = storage.load_gitlab_config(role)?;
    let overlay = crate::infra::config_overlay::load_overlay()?;
    let Some(overlay) = overlay else {
        return Ok(base);
    };
    let Some(role_overlay) = overlay.role_overlay(role) else {
        return Ok(base);
    };
    let mut merged = base.clone().unwrap_or_default();
    let mut present = base.is_some();
    if let Some(value) = &role_overlay.gitlab_api_base {
        merged.api_base = Some(value.clone());
        present = true;
    }
    if present { Ok(Some(merged)) } else { Ok(None) }
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

/// Load the admin-provisioned Redmine identity (`user_id`, `login`) for
/// `role`. Phase 2 persists one row per agent role when the deterministic
/// service user is found or created; `None` means "never provisioned".
pub fn load_redmine_user(role: Role, storage: &Storage) -> Result<Option<(u64, String)>, String> {
    storage.load_redmine_user(role)
}

/// Persist the admin-provisioned Redmine identity for `role`. The API key
/// itself stays in `role_credential`; this row only carries the non-secret
/// identity so reruns can reuse without re-listing users.
pub fn save_redmine_user(
    role: Role,
    user_id: u64,
    login: &str,
    storage: &Storage,
) -> Result<(), String> {
    storage.save_redmine_user(role, user_id, login)
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
/// to SQLite via `phasegent config set redmine-git-mirror-api-key --stdin`
/// (or the secure interactive prompt) so a long-lived deployment does
/// not have to ship the key in every shell that runs `workflow bootstrap`.
/// The environment variable still wins for one-off rotations because the
/// resolver only falls back when the env var is unset or empty.
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
/// TOML `redmine_repository_url` → SQLite `global_setting` row → absent.
/// Persisting the URL is done with `phasegent config set
/// redmine-repository-url <URL>` (or `PHASEGENT_REDMINE_REPOSITORY_URL`)
/// so a long-lived deployment does not have to ship the URL in every
/// shell that runs `workflow bootstrap`. Direct TOML edits affect this
/// same resolver path; no new command is required. The overlay is
/// read-only: `config set`/`clear` continue to touch SQLite only, so a
/// TOML value shadows a SQLite value until the TOML (or env) is removed.
/// The environment variable still wins so ad-hoc runs can override both
/// file and persisted values without rewriting either. The caller
/// supplies the [`Storage`] handle so production code can call
/// [`Storage::open`] while tests can drive the resolver against an
/// isolated temp database.
pub fn redmine_repository_url_override(storage: &Storage) -> Result<Option<String>, String> {
    if let Some(value) = read_env_trimmed("PHASEGENT_REDMINE_REPOSITORY_URL")? {
        return Ok(Some(value));
    }
    if let Some(overlay) = crate::infra::config_overlay::load_overlay()?
        && let Some(value) = overlay.redmine_repository_url_value()
    {
        return Ok(Some(value.to_owned()));
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
    close_status_id: Option<String>,
) -> Result<(), String> {
    if api_base.is_some() || close_status_id.is_some() {
        let mut config = storage.load_redmine_config(role)?.unwrap_or_default();
        if api_base.is_some() {
            config.api_base = api_base;
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

#[allow(dead_code)]
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
) -> Result<(), String> {
    if api_base.is_some() {
        let mut config = storage.load_gitlab_config(role)?.unwrap_or_default();
        config.api_base = api_base;
        storage.save_gitlab_config(role, &config)?;
    }
    storage.update_provider(role, PROVIDER_GITLAB)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    use std::fs;

    #[test]
    fn auth_setup_local_is_password_free_and_flips_role_provider() {
        // Issue 211 P1/P3: the local provider keeps no credential, needs
        // no repository and no close-status-id, so `auth setup
        // --provider local` must succeed without reading stdin or
        // prompting, and persist only the role-scoped provider
        // preference so `config show` / `resolve_kind` report `local`.
        let _lock = lock_workflow_tests();
        let dir = std::env::temp_dir().join(format!(
            "phasegent-auth-local-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join(crate::infra::storage::DB_FILENAME);
        let _db_guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());

        // read_stdin=false still must not prompt: the local arm returns
        // before touching `read_credential`.
        let out = setup_provider(
            Role::Executor,
            PROVIDER_LOCAL,
            SetupOptions {
                read_stdin: false,
                api_base: None,
                repository: None,
                close_status_id: None,
            },
        )
        .expect("local setup must not require credentials");
        assert_eq!(out["configured"], true);
        assert_eq!(out["role"], "executor");
        assert_eq!(out["provider"], "local");

        let storage = Storage::open().unwrap();
        let provider = load_config(Role::Executor, &storage)
            .unwrap()
            .and_then(|config| config.provider);
        assert_eq!(provider, Some(PROVIDER_LOCAL.to_owned()));
        // No credential row is ever written for the local provider.
        assert!(
            storage
                .load_credential(Role::Executor, PROVIDER_LOCAL)
                .is_err()
                || storage
                    .load_credential(Role::Executor, PROVIDER_LOCAL)
                    .unwrap()
                    .is_none(),
            "local must never store a credential"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn auth_setup_local_rejects_inapplicable_options() {
        // Mirrors the GitLab arms: local takes neither a Forgejo
        // repository nor a Redmine close-status-id, and the message is
        // provider-agnostic (it names the owning provider).
        let _lock = lock_workflow_tests();
        let err = setup_provider(
            Role::Executor,
            PROVIDER_LOCAL,
            SetupOptions {
                read_stdin: true,
                api_base: None,
                repository: Some("owner/repo".to_owned()),
                close_status_id: None,
            },
        )
        .unwrap_err();
        assert_eq!(err, "--repository requires the forgejo provider");

        let err = setup_provider(
            Role::Executor,
            PROVIDER_LOCAL,
            SetupOptions {
                read_stdin: true,
                api_base: None,
                repository: None,
                close_status_id: Some("1".to_owned()),
            },
        )
        .unwrap_err();
        assert_eq!(err, "--close-status-id requires the redmine provider");
    }
}
