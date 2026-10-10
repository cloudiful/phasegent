use crate::infra::storage::{
    GLOBAL_REDMINE_GIT_MIRROR_API_KEY, GLOBAL_REDMINE_REPOSITORY_URL, PROVIDER_REDMINE, Storage,
};
// `PROVIDER_LOCAL` is imported from `storage_schema` directly because the
// `storage` aggregator re-export lists only the non-local provider constants.
use crate::infra::storage_schema::PROVIDER_LOCAL;
// `GLOBAL_REDMINE_API_BASE` is imported from `storage_schema` directly so the
// canonical global key does not require widening the `storage` aggregator.
use crate::infra::storage_schema::GLOBAL_REDMINE_API_BASE;
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
    /// JSON and SQLite decoding; no longer persisted or read—resolution
    /// uses only explicit `--project-id`. The SQLite column remains for
    /// non-destructive migration but values are ignored and cleared on open.
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

pub struct SetupOptions {
    pub read_stdin: bool,
    pub api_base: Option<String>,
    pub repository: Option<String>,
    pub close_status_id: Option<String>,
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
        // The local provider keeps no credential, needs no repository and no
        // close-status-id (both rejected above), and has no backend table.
        // Flip the role-scoped provider preference only so `resolve_kind`
        // and `config show` report `local` while redmine rows stay intact.
        // `api_base`/`read_stdin` are inert: there is nowhere to persist a
        // base URL and nothing to read from stdin.
        let storage = Storage::open()?;
        storage.update_provider(role, PROVIDER_LOCAL)?;
        return Ok(serde_json::json!({
            "configured": true,
            "role": role.as_str(),
            "provider": provider
        }));
    }
    if provider != PROVIDER_REDMINE {
        return Err(format!("unsupported provider '{provider}'"));
    }
    let credential = read_credential("Redmine API key", read_stdin)?;
    let credential = credential.trim().to_owned();
    if credential.is_empty() {
        return Err("Redmine API key cannot be empty".to_owned());
    }

    let storage = Storage::open()?;
    storage.save_credential(role, provider, &credential)?;
    save_redmine_config(&storage, role, api_base, close_status_id)?;

    Ok(serde_json::json!({
        "configured": true,
        "role": role.as_str(),
        "provider": provider
    }))
}

fn validate_provider_options(
    provider: &str,
    repository: &Option<String>,
    close_status_id: &Option<String>,
) -> Result<(), String> {
    // Provider-agnostic: the messages describe which provider owns each
    // option rather than which provider was configured. Credentials are
    // not validated here; the `setup_provider` local arm skips credential
    // handling entirely.
    if provider == PROVIDER_REDMINE && repository.is_some() {
        return Err("--repository is not a Redmine option".to_owned());
    }
    if provider == PROVIDER_LOCAL && repository.is_some() {
        return Err("--repository is not a local option".to_owned());
    }
    if provider == PROVIDER_LOCAL && close_status_id.is_some() {
        return Err("--close-status-id requires the redmine provider".to_owned());
    }
    Ok(())
}

fn read_credential(label: &str, read_stdin: bool) -> Result<String, String> {
    if read_stdin {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| format!("could not read credential from stdin: {error}"))?;
        Ok(input.trim().to_owned())
    } else {
        rpassword::prompt_password(format!("{label}: "))
            .map_err(|error| format!("could not read credential securely: {error}"))
            .map(|value| value.trim().to_owned())
    }
}

pub fn load_config(role: Role, storage: &Storage) -> Result<Option<StoredConfig>, String> {
    // Effective role config: TOML overlays legacy SQLite so direct file
    // edits affect the same resolver paths used by normal commands.
    // Precedence for the provider field is TOML > SQLite; callers apply
    // explicit CLI > env before this stored-effective value. Credentials
    // never consult TOML. Overlay parse/secret errors propagate instead of
    // falling back so malformed TOML cannot be silently ignored.
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
    if present { Ok(Some(merged)) } else { Ok(None) }
}

pub fn load_redmine_config(
    role: Role,
    storage: &Storage,
) -> Result<Option<RedmineStoredConfig>, String> {
    // Effective legacy Redmine config: TOML > SQLite, same contract as
    // above. The `api_base` field is a legacy read/migration input only:
    // runtime address resolution goes through [`redmine_api_base`], and new
    // writes never populate it. `close_status_id` stays role-scoped.
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

/// Roles scanned by the bounded legacy Redmine address migration.
const REDMINE_ROLES: [Role; 5] = [
    Role::Admin,
    Role::Orchestrator,
    Role::Executor,
    Role::Reviewer,
    Role::Explore,
];

/// Resolve the canonical global Redmine REST API base.
///
/// Precedence, highest first (callers apply an explicit `--api-base`
/// above this):
///   1. `PHASEGENT_REDMINE_API_BASE` environment variable (Redmine
///      runtime override).
///   2. `PHASEGENT_API_BASE` environment variable (generic runtime
///      compatibility alias).
///   3. TOML top-level `redmine_api_base`.
///   4. Persisted `global_setting` row `PHASEGENT_REDMINE_API_BASE`.
///   5. Bounded legacy migration: when no canonical value exists yet,
///      the effective per-role addresses (`role_redmine_config.api_base`
///      overlaid by `[roles.<role>] redmine_api_base`, which is what
///      [`load_redmine_config`] returns) are normalised. A single
///      distinct address is migrated into the global setting and
///      returned; distinct addresses fail closed so no role, first
///      value, or credential-derived value is ever chosen.
///
/// The address is non-secret. Credentials, identities, provider
/// selection, close-status ids, and the mirror bearer key keep their
/// independent paths and are never consulted here. Environment and CLI
/// values are runtime overrides only and are never persisted; only the
/// legacy migration writes the global row.
pub fn redmine_api_base(storage: &Storage) -> Result<Option<String>, String> {
    if let Some(value) = read_env_trimmed("PHASEGENT_REDMINE_API_BASE")? {
        return Ok(Some(value));
    }
    if let Some(value) = read_env_trimmed("PHASEGENT_API_BASE")? {
        return Ok(Some(value));
    }
    if let Some(value) = crate::infra::config_overlay::load_overlay()?
        .and_then(|overlay| overlay.redmine_api_base_value().map(str::to_owned))
    {
        return Ok(Some(value));
    }
    if let Some(value) = storage.load_global_setting(GLOBAL_REDMINE_API_BASE)? {
        return Ok(Some(value));
    }
    migrate_legacy_redmine_api_base(storage)
}

/// Migrate the single legacy per-role Redmine address into the canonical
/// global setting, or fail closed when the legacy values conflict.
///
/// Returns `Ok(None)` when no role carries a non-empty legacy address so
/// the caller can keep its "not configured" diagnostic. Normalisation
/// reuses the same Redmine URL rules as runtime resolution, and only a
/// successful migration persists anything.
fn migrate_legacy_redmine_api_base(storage: &Storage) -> Result<Option<String>, String> {
    let mut distinct: Vec<String> = Vec::new();
    let mut roles_with_values: Vec<&'static str> = Vec::new();
    for role in REDMINE_ROLES {
        let Some(config) = load_redmine_config(role, storage)? else {
            continue;
        };
        let Some(raw) = config.api_base.as_deref() else {
            continue;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Reject credential-bearing/invalid legacy addresses before they
        // can reach the canonical non-sensitive global row. The error is
        // bounded and never echoes the address.
        validate_redmine_api_base(trimmed).map_err(|_| {
            format!(
                "legacy Redmine API base for role '{}' is invalid; \
                 set the global redmine_api_base (config set redmine-api-base)",
                role.as_str()
            )
        })?;
        let normalized = crate::remote::normalize_redmine_api_base(trimmed).map_err(|_| {
            format!(
                "legacy Redmine API base for role '{}' is invalid; \
                 set the global redmine_api_base (config set redmine-api-base)",
                role.as_str()
            )
        })?;
        if !distinct.iter().any(|existing| existing == &normalized) {
            distinct.push(normalized);
        }
        roles_with_values.push(role.as_str());
    }
    match distinct.len() {
        0 => Ok(None),
        1 => {
            let value = distinct.pop().expect("one distinct legacy address");
            storage.save_global_setting(GLOBAL_REDMINE_API_BASE, &value)?;
            Ok(Some(value))
        }
        _ => Err(format!(
            "conflicting legacy Redmine API base addresses across roles ({}); \
             set the global redmine_api_base (config set redmine-api-base) to one address",
            roles_with_values.join(", ")
        )),
    }
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

/// Persist the `admin auth setup --provider redmine` fields: the
/// non-secret REST address routes to the canonical global setting, the
/// close-status id stays role-scoped, and the role provider preference is
/// flipped to `redmine`. Exposed `pub(crate)` so focused config regression
/// tests can drive the exact write path without a credential prompt.
pub(crate) fn save_redmine_config(
    storage: &Storage,
    role: Role,
    api_base: Option<String>,
    close_status_id: Option<String>,
) -> Result<(), String> {
    if let Some(value) = api_base {
        // The canonical Redmine REST address is global and non-secret, so
        // `auth setup --provider redmine --api-base` writes the global
        // setting instead of a role-scoped address row. Validation runs
        // before any write and never echoes the value.
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("Redmine API base cannot be empty".to_owned());
        }
        validate_redmine_api_base(trimmed)?;
        storage.save_global_setting(GLOBAL_REDMINE_API_BASE, trimmed)?;
    }
    if let Some(value) = close_status_id {
        // Only the role-scoped close-status id is written here. A
        // pre-existing legacy address row is left untouched (inert) rather
        // than rewritten; setup never originates a role-scoped address.
        let mut config = storage.load_redmine_config(role)?.unwrap_or_default();
        config.close_status_id = Some(
            value
                .parse()
                .map_err(|_| "Redmine close status id must be numeric".to_owned())?,
        );
        storage.save_redmine_config(role, &config)?;
    }
    storage.update_provider(role, PROVIDER_REDMINE)
}

/// Validate a non-sensitive Redmine REST base before it is persisted into
/// the canonical global setting. Rejects a non-http(s) scheme, a missing
/// host, userinfo (embedded credentials), and a query or fragment. Every
/// error is bounded and never echoes the input so a credential-bearing
/// value cannot leak through the diagnostic.
fn validate_redmine_api_base(value: &str) -> Result<(), String> {
    let parsed =
        url::Url::parse(value).map_err(|_| "Redmine API base is not a valid URL".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Redmine API base must use http or https".to_owned());
    }
    if parsed.host_str().is_none() {
        return Err("Redmine API base must include a host".to_owned());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Redmine API base must not contain credentials".to_owned());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("Redmine API base must not contain a query or fragment".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
    use std::fs;

    #[test]
    fn auth_setup_local_is_password_free_and_flips_role_provider() {
        // The local provider keeps no credential, needs no repository and no
        // close-status-id, so `auth setup --provider local` must succeed
        // without reading stdin or prompting, and persist only the
        // role-scoped provider preference so `config show` / `resolve_kind`
        // report `local`.
        let _lock = lock_workflow_tests();
        let dir = crate::test_scratch::root().join(format!(
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
        // Local takes neither a repository nor a Redmine close-status-id,
        // and the message names the owning provider.
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
        assert_eq!(err, "--repository is not a local option");

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
