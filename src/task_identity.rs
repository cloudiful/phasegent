//! Durable canonical task identity storage (issue 1201 P5).
//!
//! A task is identified by the immutable four-tuple
//! `(provider, instance, project, external_id)` and addressed by an opaque
//! local `task_id`. This module only owns storage: it adds no runtime
//! resolver and no command calls it yet. P6 wires the reader/writer into
//! creation, binding and numbered lookups.

use rusqlite::{OptionalExtension, TransactionBehavior};

use crate::infra::storage::Storage;
use crate::providers::ProviderKind;

const MAX_FIELD_CHARS: usize = 512;

/// An immutable canonical task identity.
///
/// Construct through [`TaskKey::new`], which trims inputs, lowercases the
/// provider, normalizes the instance endpoint, and validates the canonical
/// positive decimal external id. The fields stay private so a malformed key
/// can never reach SQL directly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskKey {
    provider: String,
    instance: String,
    project: String,
    external_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskBinding {
    task_id: i64,
    key: TaskKey,
    created_at: i64,
}

impl TaskKey {
    pub(crate) fn new(
        provider: &str,
        instance: &str,
        project: &str,
        external_id: &str,
    ) -> Result<Self, String> {
        let provider_raw = validate_field(provider, "provider")?.to_ascii_lowercase();
        let kind = provider_raw
            .parse::<ProviderKind>()
            .map_err(|error| format!("invalid task provider: {error}"))?;
        let project = validate_field(project, "project")?;
        let external_id = validate_external_id(external_id)?;
        if kind == ProviderKind::Local {
            if validate_field(instance, "instance")? != "local" {
                return Err("Local task instance must be 'local'".to_owned());
            }
            let instance = canonical_instance(kind, None, &project)?;
            return Ok(Self {
                provider: provider_raw,
                instance,
                project,
                external_id,
            });
        }
        let instance = canonical_instance(kind, Some(instance), &project)?;
        Ok(Self {
            provider: provider_raw,
            instance,
            project,
            external_id,
        })
    }

    pub(crate) fn external_number(&self) -> Option<u64> {
        self.external_id.parse().ok()
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }

    pub(crate) fn project(&self) -> &str {
        &self.project
    }

    pub(crate) fn external_id(&self) -> &str {
        &self.external_id
    }
}

impl TaskBinding {
    pub(crate) fn task_id(&self) -> i64 {
        self.task_id
    }

    pub(crate) fn key(&self) -> &TaskKey {
        &self.key
    }
}

/// Normalize a non-secret instance endpoint into its canonical identity.
///
/// Local is the credential-free sentinel `local`; Redmine, Forgejo, and GitLab keep
/// their validated http(s) base, including any significant port and subpath,
/// with a trailing slash removed. Credentials, query strings and fragments
/// are never accepted or stored.
pub(crate) fn canonical_instance(
    provider: ProviderKind,
    endpoint: Option<&str>,
    project: &str,
) -> Result<String, String> {
    match provider {
        ProviderKind::Local => {
            if project != "default" {
                return Err("Local task project must be 'default'".to_owned());
            }
            Ok("local".to_owned())
        }
        ProviderKind::Forgejo => {
            let endpoint = endpoint
                .ok_or_else(|| "Forgejo task identity requires an instance endpoint".to_owned())?;
            let config = crate::providers::forgejo::ForgejoConfig::new(endpoint, project).map_err(
                |error| {
                    format!(
                        "invalid Forgejo task instance: {}",
                        error
                            .json()
                            .get("message")
                            .and_then(|value| value.as_str())
                            .unwrap_or("invalid endpoint or project")
                    )
                },
            )?;
            Ok(config.api_base().to_owned())
        }
        ProviderKind::Gitlab => {
            let endpoint = endpoint
                .ok_or_else(|| "GitLab task identity requires an instance endpoint".to_owned())?;
            let config = crate::providers::gitlab::GitlabConfig::new(endpoint, project).map_err(
                |error| {
                    format!(
                        "invalid GitLab task instance: {}",
                        error
                            .json()
                            .get("message")
                            .and_then(|value| value.as_str())
                            .unwrap_or("invalid endpoint or project")
                    )
                },
            )?;
            Ok(config.api_base().to_owned())
        }
        ProviderKind::Redmine => {
            let endpoint = endpoint
                .ok_or_else(|| "Redmine task identity requires an instance endpoint".to_owned())?;
            let url = url::Url::parse(endpoint)
                .map_err(|error| format!("invalid Redmine task instance: {error}"))?;
            if !url.username().is_empty() || url.password().is_some() {
                return Err("Redmine task instance must not include userinfo".to_owned());
            }
            crate::remote::normalize_redmine_api_base(endpoint)
                .map_err(|error| format!("invalid Redmine task instance: {error}"))
        }
    }
}

fn validate_field(value: &str, field: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("task {field} must be non-empty"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(format!("task {field} must not contain control characters"));
    }
    if trimmed.chars().count() > MAX_FIELD_CHARS {
        return Err(format!("task {field} is too long"));
    }
    Ok(trimmed.to_owned())
}

fn validate_external_id(value: &str) -> Result<String, String> {
    let trimmed = validate_field(value, "external id")?;
    if trimmed.len() > 1 && trimmed.starts_with('0') {
        return Err("task external id must not have leading zeros".to_owned());
    }
    let number = trimmed
        .parse::<u64>()
        .map_err(|_| "task external id must be a positive decimal number".to_owned())?;
    if number == 0 || number > i64::MAX as u64 {
        return Err("task external id must be a positive decimal number".to_owned());
    }
    Ok(trimmed)
}

fn validate_now(now: i64) -> Result<(), String> {
    if now <= 0 {
        return Err("task timestamp must be greater than zero".to_owned());
    }
    Ok(())
}

fn validate_repo_key(repo_key: &str) -> Result<&str, String> {
    if repo_key.trim().is_empty() {
        return Err("task repo key must be non-empty".to_owned());
    }
    if repo_key.chars().any(char::is_control) {
        return Err("task repo key must not contain control characters".to_owned());
    }
    if repo_key.chars().count() > MAX_FIELD_CHARS {
        return Err("task repo key is too long".to_owned());
    }
    Ok(repo_key)
}

#[cfg(test)]
impl TaskBinding {
    pub(crate) fn for_test(key: TaskKey, task_id: i64, created_at: i64) -> Self {
        Self {
            task_id,
            key,
            created_at,
        }
    }
}

fn select_binding(
    connection: &rusqlite::Connection,
    key: &TaskKey,
) -> Result<Option<TaskBinding>, String> {
    let row: Option<(i64, String, String, String, String, i64)> = connection
        .query_row(
            "SELECT task_id, provider, instance, project, external_id, created_at \
               FROM task_bindings \
              WHERE provider = ?1 AND instance = ?2 AND project = ?3 AND external_id = ?4",
            rusqlite::params![key.provider, key.instance, key.project, key.external_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("could not read task binding: {error}"))?;
    row.map(decode_binding).transpose()
}

fn decode_binding(
    (task_id, provider, instance, project, external_id, created_at): (
        i64,
        String,
        String,
        String,
        String,
        i64,
    ),
) -> Result<TaskBinding, String> {
    if task_id <= 0 {
        return Err("stored task binding has a non-positive task id".to_owned());
    }
    if created_at <= 0 {
        return Err("stored task binding has an invalid timestamp".to_owned());
    }
    let key = TaskKey::new(&provider, &instance, &project, &external_id)
        .map_err(|error| format!("stored task binding is corrupt: {error}"))?;
    if key.provider != provider
        || key.instance != instance
        || key.project != project
        || key.external_id != external_id
    {
        return Err("stored task binding is not canonical".to_owned());
    }
    Ok(TaskBinding {
        task_id,
        key,
        created_at,
    })
}

/// Return the binding for `key`, creating the canonical row when absent.
///
/// One immediate transaction serializes concurrent inserters: the loser's
/// `ON CONFLICT DO NOTHING` is a no-op and it reads the winner's `task_id`,
/// so separate connections converge on one row without a lock error under
/// the established busy-timeout policy.
pub(crate) fn ensure_binding(
    storage: &Storage,
    key: &TaskKey,
    now: i64,
) -> Result<TaskBinding, String> {
    validate_now(now)?;
    let transaction =
        rusqlite::Transaction::new_unchecked(&storage.connection, TransactionBehavior::Immediate)
            .map_err(|error| format!("could not begin task binding transaction: {error}"))?;
    let binding = ensure_binding_with(&transaction, key, now)?;
    transaction
        .commit()
        .map_err(|error| format!("could not commit task binding: {error}"))?;
    Ok(binding)
}

/// Transaction-compatible binding insert/read so an adopter can persist the
/// task, membership and branch reference atomically on one connection.
pub(crate) fn ensure_binding_with(
    connection: &rusqlite::Connection,
    key: &TaskKey,
    now: i64,
) -> Result<TaskBinding, String> {
    validate_now(now)?;
    connection
        .execute(
            "INSERT INTO task_bindings (provider, instance, project, external_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(provider, instance, project, external_id) DO NOTHING",
            rusqlite::params![
                key.provider,
                key.instance,
                key.project,
                key.external_id,
                now
            ],
        )
        .map_err(|error| format!("could not insert task binding: {error}"))?;
    select_binding(connection, key)?
        .ok_or_else(|| "task binding disappeared during insert".to_owned())
}

/// Add `(repo_key, task_id)` membership. A duplicate is a no-op, while a
/// missing referenced binding is rejected inside the same transaction (the
/// check does not rely on foreign-key enforcement being enabled).
pub(crate) fn add_repo_membership(
    storage: &Storage,
    repo_key: &str,
    task_id: i64,
    now: i64,
) -> Result<(), String> {
    let transaction =
        rusqlite::Transaction::new_unchecked(&storage.connection, TransactionBehavior::Immediate)
            .map_err(|error| format!("could not begin task membership transaction: {error}"))?;
    add_repo_membership_with(&transaction, repo_key, task_id, now)?;
    transaction
        .commit()
        .map_err(|error| format!("could not commit task membership: {error}"))?;
    Ok(())
}

/// Transaction-compatible membership insert with an explicit referenced
/// binding check, so the caller's adoption transaction stays all-or-nothing
/// even when SQLite foreign-key enforcement is absent.
pub(crate) fn add_repo_membership_with(
    connection: &rusqlite::Connection,
    repo_key: &str,
    task_id: i64,
    now: i64,
) -> Result<(), String> {
    let repo_key = validate_repo_key(repo_key)?;
    validate_now(now)?;
    if task_id <= 0 {
        return Err("task id must be greater than zero".to_owned());
    }
    let exists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM task_bindings WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("could not verify task binding: {error}"))?;
    if exists != 1 {
        return Err(format!("task binding {task_id} does not exist"));
    }
    connection
        .execute(
            "INSERT INTO task_repo_memberships (repo_key, task_id, created_at) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(repo_key, task_id) DO NOTHING",
            rusqlite::params![repo_key, task_id, now],
        )
        .map_err(|error| format!("could not insert task membership: {error}"))?;
    Ok(())
}

/// Look up a binding by its opaque local id, validating the persisted row.
pub(crate) fn binding_for_task_id(
    storage: &Storage,
    task_id: i64,
) -> Result<Option<TaskBinding>, String> {
    binding_for_task_id_on(&storage.connection, task_id)
}

/// Look up a binding by its opaque local id on an arbitrary connection, so a
/// transaction-bound reader can validate a persisted reference without
/// opening a second store.
pub(crate) fn binding_for_task_id_on(
    connection: &rusqlite::Connection,
    task_id: i64,
) -> Result<Option<TaskBinding>, String> {
    if task_id <= 0 {
        return Err("task id must be greater than zero".to_owned());
    }
    let row: Option<(i64, String, String, String, String, i64)> = connection
        .query_row(
            "SELECT task_id, provider, instance, project, external_id, created_at \
               FROM task_bindings WHERE task_id = ?1",
            [task_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("could not read task binding: {error}"))?;
    row.map(decode_binding).transpose()
}

/// Every explicit membership binding for one bare number in one physical
/// repository, ordered by `task_id`. Zero, one or many rows are returned so
/// P6 can decide ambiguity; no global number lookup happens here.
pub(crate) fn bindings_for_repo_number(
    storage: &Storage,
    repo_key: &str,
    number: u64,
) -> Result<Vec<TaskBinding>, String> {
    bindings_for_repo_number_with(&storage.connection, repo_key, number)
}

/// Transaction-compatible repo-number membership read used by adoption
/// conflict checks before any row is written.
pub(crate) fn bindings_for_repo_number_with(
    connection: &rusqlite::Connection,
    repo_key: &str,
    number: u64,
) -> Result<Vec<TaskBinding>, String> {
    let repo_key = validate_repo_key(repo_key)?;
    if number == 0 || number > i64::MAX as u64 {
        return Err("task repo number must be a positive decimal number".to_owned());
    }
    let mut statement = connection
        .prepare(
            "SELECT b.task_id, b.provider, b.instance, b.project, b.external_id, b.created_at \
               FROM task_repo_memberships m \
               JOIN task_bindings b ON b.task_id = m.task_id \
              WHERE m.repo_key = ?1 AND b.external_id = ?2 \
              ORDER BY b.task_id",
        )
        .map_err(|error| format!("could not prepare task repo lookup: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![repo_key, number.to_string()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(|error| format!("could not read task repo lookup: {error}"))?;
    let mut bindings = Vec::new();
    for row in rows {
        let raw = row.map_err(|error| format!("could not read task repo row: {error}"))?;
        bindings.push(decode_binding(raw)?);
    }
    Ok(bindings)
}

/// Every durable task for one bare number, across every repository.
///
/// A number-only command (a timer names only an issue number) has no
/// repository to scope the lookup, so this returns the complete set: zero,
/// one or many tasks. Several tasks for one number are an ambiguity the
/// caller must refuse rather than guess.
pub(crate) fn bindings_for_number(
    connection: &rusqlite::Connection,
    number: u64,
) -> Result<Vec<TaskBinding>, String> {
    if number == 0 || number > i64::MAX as u64 {
        return Err("task number must be a positive decimal number".to_owned());
    }
    let mut statement = connection
        .prepare(
            "SELECT b.task_id, b.provider, b.instance, b.project, b.external_id, b.created_at \
               FROM task_bindings b \
              WHERE b.external_id = ?1 \
              ORDER BY b.task_id",
        )
        .map_err(|error| format!("could not prepare task number lookup: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![number.to_string()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(|error| format!("could not read task number lookup: {error}"))?;
    let mut bindings = Vec::new();
    for row in rows {
        let raw = row.map_err(|error| format!("could not read task number row: {error}"))?;
        bindings.push(decode_binding(raw)?);
    }
    Ok(bindings)
}

/// Whether the task is an explicit member of the physical repository.
///
/// The lookup runs on the caller's own connection, so a membership proof
/// inside an immediate transaction reads exactly the state that transaction
/// holds. A missing membership table or column is absent evidence, not a
/// membership.
pub(crate) fn task_repo_membership_exists_on(
    connection: &rusqlite::Connection,
    repo_key: &str,
    task_id: i64,
) -> Result<bool, String> {
    validate_repo_key(repo_key)?;
    if task_id <= 0 {
        return Err("task id must be greater than zero".to_owned());
    }
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM task_repo_memberships m \
               JOIN task_bindings b ON b.task_id = m.task_id \
              WHERE m.repo_key = ?1 AND m.task_id = ?2",
            rusqlite::params![repo_key, task_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("could not read task repo membership: {error}"))?;
    Ok(count == 1)
}
